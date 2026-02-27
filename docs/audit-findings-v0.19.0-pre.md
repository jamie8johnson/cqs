# Audit Findings — v0.14.0

Generated: 2026-02-22

## Documentation

**Result:** All documentation for v0.14.0 is complete and consistent. No findings.

### Verification Summary

- ✅ **README.md**: Documents `cqs task` with complete command description and RAG efficiency metrics (line 480)
- ✅ **CONTRIBUTING.md**: Architecture overview lists task.rs in commands (line 92) and includes library-level task.rs in overview (line 163)
- ✅ **CHANGELOG.md**: v0.14.0 section includes comprehensive entry for `cqs task` feature, scout gap detection, and retrieval metrics
- ✅ **SECURITY.md**: No new security-relevant behavior for task command to document
- ✅ **CLAUDE.md**: Comprehensive command documentation at line 75, including waterfall token budgeting explanation (line 95)
- ✅ **src/lib.rs**: Properly exports task module (line 79) and re-exports public types (line 126: `task`, `task_to_json`, `TaskResult`, `TaskSummary`)
- ✅ **src/cli/commands/task.rs**: Has proper module doc comment (line 1: "Task command — one-shot implementation context for a task description")
- ✅ **src/task.rs**: Has proper module-level doc comment (lines 1-4) describing scout + gather + impact + placement + notes integration

### Cross-Documentation Consistency

Checked command definition in src/cli/mod.rs (lines 610-622):
- Task command struct has all documented flags: `description`, `limit (-n)`, `json`, `tokens`
- Signature matches all documentation examples

All examples in README are syntactically correct and match the CLI argument definitions.

## API Design

#### AD-1: TaskResult and TaskSummary missing standard derives
- **Difficulty:** easy
- **Location:** `src/task.rs:20-45`
- **Description:** `TaskResult` and `TaskSummary` have no `#[derive(...)]` attributes. Every peer result type in the codebase derives at minimum `Debug` and `Clone`: `ImpactResult` (`Debug, Clone, Serialize`), `OnboardResult` (`Debug, Clone, Serialize`), `DiffImpactResult` (`Debug, Clone, Serialize`), `RelatedResult` (`Debug, Clone`), `GatheredChunk` (via `GatherOptions` `Debug`). Missing `Debug` means these types can't be used in `tracing` debug output or `assert_eq!` in tests without manual formatting.
- **Suggested fix:** Add `#[derive(Debug, Clone)]` to both structs. `Serialize` can be skipped since JSON output is handled by `task_to_json()` manual construction.

#### AD-2: ScoutChunk, FileGroup, ScoutSummary, ScoutResult missing standard derives
- **Difficulty:** easy
- **Location:** `src/scout.rs:26-70`
- **Description:** Same pattern as AD-1. These four structs have no derives (only `ChunkRole` has `#[derive(Debug, Clone, PartialEq, Eq)]`). Peer summary types like `OnboardSummary`, `DiffImpactSummary`, `FunctionHints` all derive `Debug, Clone, Serialize`. `ScoutResult` in particular is embedded in `TaskResult`, so both structs inherit the missing-derives problem.
- **Suggested fix:** Add `#[derive(Debug, Clone)]` to all four structs. If serde serialization is ever needed directly (instead of via `scout_to_json`), add `Serialize` later.

#### AD-3: PlacementResult, FileSuggestion, LocalPatterns, PlacementOptions, ScoutOptions missing standard derives
- **Difficulty:** easy
- **Location:** `src/where_to_add.rs:21-53,62`, `src/scout.rs:84`
- **Description:** Continuation of AD-1/AD-2. `GatherOptions` derives `Debug` but `ScoutOptions` and `PlacementOptions` do not. `PlacementResult`, `FileSuggestion`, `LocalPatterns` have no derives. These are all public types re-exported from `lib.rs`.
- **Suggested fix:** Add `#[derive(Debug, Clone)]` to all five structs. Consistent with `GatherOptions` which already has `Debug`.

#### AD-4: risk_level/blast_radius serialized via Debug format instead of Display
- **Difficulty:** easy
- **Location:** `src/task.rs:255-256`, `src/cli/commands/task.rs:357-359`
- **Description:** Both `task_to_json()` and `build_risk_json()` serialize `RiskLevel` using `format!("{:?}", r.risk_level)` which produces PascalCase (`"High"`, `"Medium"`, `"Low"`). However, `RiskLevel` has `#[serde(rename_all = "lowercase")]` and a `Display` impl that outputs lowercase (`"high"`, `"medium"`, `"low"`). This means: (1) `task` JSON output uses `"High"` while a direct `serde_json::to_value(&risk_score)` would produce `"high"`, and (2) the text output in `print_impact_section_idx` uses `format!("{:?}", r.risk_level)` too. JSON consumers see inconsistent casing depending on whether they get the field via task output vs direct serialization.
- **Suggested fix:** Replace `format!("{:?}", r.risk_level)` with `r.risk_level.to_string()` (uses Display) or `serde_json::to_value(&r.risk_level).unwrap()` (uses serde). Both produce lowercase, consistent with the type's own serialization contract.

#### AD-5: ScoutChunk uses u64 for caller_count/test_count while all peers use usize
- **Difficulty:** easy
- **Location:** `src/scout.rs:38-40`
- **Description:** `ScoutChunk.caller_count` and `ScoutChunk.test_count` are `u64`, but every other struct in the codebase uses `usize` for these fields: `FunctionHints` (`usize`), `RiskScore` (`usize`), `compute_hints_with_graph()` returns `usize`. This forces `as u64` casts at the boundary (line 248-249) and `as usize` casts would be needed going the other direction. Since these are counts of callers (never exceeding a few thousand), `u64` is unnecessary precision and creates friction.
- **Suggested fix:** Change both fields to `usize` to match `FunctionHints` and `RiskScore`. Remove the `as u64` casts at lines 248-249.

## Observability

#### OB-1: `compute_modify_threshold` result not logged — threshold invisible in traces
- **Difficulty:** easy
- **Location:** `src/scout.rs:308` (`compute_modify_threshold`), called from `scout_core` at line 216
- **Description:** The modify/dependency threshold is a key classification decision. `compute_modify_threshold` returns a float that determines which chunks become ModifyTarget vs Dependency, but neither the function itself nor its caller logs the computed value. When users ask "why was function X classified as Dependency?", the answer is invisible in logs. The gap ratio (`best_gap`) and split index are also unlogged — the entire decision is a black box.
- **Suggested fix:** Add `tracing::debug!(threshold, gap = best_gap, scores = scores.len(), "Modify threshold computed")` before the return in `compute_modify_threshold`, or log it in `scout_core` after the call at line 216.

#### OB-2: `scout_core` missing search result count in logs
- **Difficulty:** easy
- **Location:** `src/scout.rs:154-299` (`scout_core`)
- **Description:** `scout_core` performs the search at line 162 but never logs how many results came back from `search_filtered`. When called from `task()`, the parent logs file_groups and functions after grouping, but the raw search result count (before grouping/truncation) is lost. This matters for debugging "why did task return only 2 files?" — was it 2 search results, or 15 results that grouped into 2 files?
- **Suggested fix:** Add `tracing::debug!(results = results.len(), "Scout search complete")` after the search at line 167, before the empty-results early return.

#### OB-3: `dispatch_task` batch token budgeting not logged
- **Difficulty:** easy
- **Location:** `src/cli/batch/handlers.rs:1460-1493` (`dispatch_task`)
- **Description:** The CLI's `output_with_budget` logs a detailed waterfall breakdown (`tracing::info!` at cli/commands/task.rs:198 with per-section token counts). But the batch handler's simplified code-only packing at lines 1460-1493 doesn't log anything about what was packed vs total. A user running `cqs batch` with `task --tokens 500` can't see in logs how many code chunks were kept or dropped.
- **Suggested fix:** Add `tracing::debug!(packed = packed_idx.len(), total = result.code.len(), tokens_used = used, budget, "Batch task token packing")` after the `token_pack` call at line 1463.

## Error Handling

#### EH-1: `scout_with_options` hard-fails on `find_test_chunks`, `task()` gracefully degrades — inconsistent
- **Difficulty:** easy
- **Location:** `src/scout.rs:127` vs `src/task.rs:67-73`
- **Description:** `scout_with_options()` propagates `find_test_chunks()` failure with `?`, causing the entire scout to fail if test chunk loading hits a store error. In contrast, `task()` wraps the same call in a `match` with `tracing::warn!` and continues with an empty vec. Since `scout()` is also called standalone from `dispatch_scout` in batch mode, a transient test-chunk error kills the whole command. Test chunks are supplementary data (used for caller/test hints), not essential for the core search+group logic.
- **Suggested fix:** Match `task()`'s pattern — wrap `find_test_chunks()` in `scout_with_options` with `match`/`tracing::warn!` and fall back to empty vec.

#### EH-2: `dispatch_test_map` chain reconstruction uses `.unwrap_or_default()` without tracing
- **Difficulty:** easy
- **Location:** `src/cli/batch/handlers.rs:644-647`
- **Description:** In `dispatch_test_map`, the BFS chain reconstruction loop does `ancestors.get(&current).map(|(_, p)| p.clone()).unwrap_or_default()`. If `.get()` returns `None` (shouldn't happen if BFS is correct, but could if graph data is inconsistent), this silently produces an empty string, which then satisfies the `while !current.is_empty()` exit condition. The chain is silently truncated with no log. Per project convention, `.unwrap_or_default()` should not silently swallow potential issues.
- **Suggested fix:** Replace with a match that logs `tracing::warn!` if the ancestor lookup fails unexpectedly, then breaks the loop.

#### EH-3: `dispatch_onboard` and `dispatch_scout` use `.unwrap()` after `.is_none()` guard
- **Difficulty:** easy
- **Location:** `src/cli/batch/handlers.rs:1037`, `src/cli/batch/handlers.rs:1124`
- **Description:** Both `dispatch_onboard` and `dispatch_scout` use the pattern `if tokens.is_none() { return ...; } let budget = tokens.unwrap();`. While logically safe (the `is_none` check guarantees `Some`), `.unwrap()` is prohibited in non-test code by project convention. If the code is refactored and the guard is accidentally removed, this becomes a panic.
- **Suggested fix:** Use `let Some(budget) = tokens else { return ... };` (let-else) or match the `if let Some(budget) = tokens { ... }` pattern already used in other handlers like `dispatch_search` (line 131) and `dispatch_gather` (line 499).

#### EH-4: `scout_core` uses `to_str().unwrap_or("")` for path origins — empty string passed to staleness check
- **Difficulty:** easy
- **Location:** `src/scout.rs:202`
- **Description:** `file_map.keys().map(|p| p.to_str().unwrap_or(""))` converts non-UTF8 paths to empty strings. These empty strings are then passed to `check_origins_stale()`, which will either match nothing (harmless) or could match against an empty-string origin in the store (unlikely but undefined). No warning is logged for the non-UTF8 fallback.
- **Suggested fix:** Filter out non-UTF8 paths with `.filter_map(|p| p.to_str())` instead of mapping them to empty strings, or log a `tracing::debug!` when a path fails UTF-8 conversion.

#### EH-5: `dispatch_task` batch handler uses simplified token budgeting — code-only packing diverges from CLI behavior
- **Difficulty:** medium
- **Location:** `src/cli/batch/handlers.rs:1459` (comment: "simplified: pack code section only")
- **Description:** The batch `dispatch_task` handler applies `--tokens` budget only to the code section, while the CLI `cmd_task` in `src/cli/commands/task.rs:67-228` uses full waterfall budgeting across 5 sections (scout 15%, code 50%, impact 15%, placement 10%, notes 10%). This means `cqs batch` and `cqs task --tokens N` produce materially different output for the same inputs — batch retains all scout/risk/test/placement/notes untruncated while only packing code, potentially exceeding the stated token budget significantly. The `token_count` field in the JSON response only reflects code tokens, misleading consumers about actual output size.
- **Suggested fix:** Either extract the waterfall logic from `cmd_task` into a shared function and call it from both code paths, or document the simplified behavior explicitly in the batch output (e.g., add a `"token_packing": "code_only"` field). The former is preferred for consistency.

#### EH-6: `AnalysisError` has no variant for general phase failures — forces misuse of existing variants
- **Difficulty:** easy
- **Location:** `src/lib.rs:142-149`
- **Description:** `AnalysisError` has only 3 variants: `Store`, `Embedder`, `NotFound`. If `task()` or `scout()` needed to report a non-embedding, non-store error (e.g., a placement phase failure that isn't from the store layer), it would have to misuse `Embedder(msg)` or wrap in `NotFound(msg)`. Previous audit (EH-26, v0.13.1) caught this exact misuse in `onboard`. Currently `task()` works around this by catching placement errors and degrading gracefully (line 136-142), but this is only viable because placement is optional. A phase that's required would have no correct error variant.
- **Suggested fix:** Add an `AnalysisError::Other(String)` variant or `AnalysisError::Phase { phase: &'static str, source: Box<dyn std::error::Error + Send + Sync> }`. Low urgency while all current callers can degrade, but prevents the next misuse.

## Code Quality

#### CQ-1: `task_to_json` duplicates notes in JSON output
- **Difficulty:** easy
- **Location:** `src/task.rs:227-322`
- **Description:** `task_to_json` embeds `scout_to_json()` output (which includes `relevant_notes` at scout.rs:445) into the `scout` key, then also serializes `result.scout.relevant_notes` into a separate top-level `notes` key (task.rs:292-303). The JSON output contains the identical notes data twice: once under `scout.relevant_notes` and once under `notes`. This wastes tokens for consumers using `--tokens` budgeting and creates a maintenance risk — if notes serialization changes in `scout_to_json`, the top-level `notes` array could diverge. The CLI budgeted path (cli/commands/task.rs) avoids this by using `build_scout_json` (which omits notes) plus a separate `build_notes_json`. Only the library's `task_to_json` has the duplication.
- **Suggested fix:** Either remove the top-level `notes` key from `task_to_json` (consumers use `scout.relevant_notes`), or strip `relevant_notes` from the embedded scout JSON before inclusion. The former is simpler.

#### CQ-2: GatheredChunk/risk/placement/tests JSON serialization duplicated 3-6x across modules
- **Difficulty:** medium
- **Location:** `src/task.rs:230-290`, `src/cli/commands/task.rs:326-420`, `src/cli/batch/handlers.rs:1472-1489`, `src/cli/commands/gather.rs:130-140`, `src/impact/format.rs:50-60`
- **Description:** The same `serde_json::json!` block for `GatheredChunk` (name, file, line_start, line_end, language, chunk_type, signature, content, score, depth) appears in 6 locations. `RiskScore` JSON construction (risk_level, blast_radius, score, caller_count, test_count, coverage) appears in 3 locations. `FileSuggestion` JSON (file, score, insertion_line, near_function, reason) appears in 4 locations. `TestInfo` JSON (name, file, line, call_depth) appears in 3 locations. Each copy has to stay in sync manually — the `Debug` vs `Display` formatting inconsistency in AD-4 is a direct consequence. Adding a field to any of these structs requires updating 3-6 places.
- **Suggested fix:** Add `impl GatheredChunk { pub fn to_json(&self, root: &Path) -> serde_json::Value }` methods (or derive Serialize with `#[serde(rename)]` where needed) and call from all sites. Same for `RiskScore`, `FileSuggestion`, `TestInfo`.

#### CQ-3: `dispatch_task` bypasses `BatchContext` call graph cache
- **Difficulty:** easy
- **Location:** `src/cli/batch/handlers.rs:1446-1496`
- **Description:** `dispatch_task` calls `cqs::task()` (the library function), which internally calls `store.get_call_graph()` at task.rs:66. This is a direct SQLite query that builds the full call graph from scratch. Meanwhile, `BatchContext` has a `call_graph()` method (batch/mod.rs:174) backed by `OnceLock` that caches the graph for the entire session. Other batch handlers that need the graph (e.g., `dispatch_test_map`, `dispatch_trace`) use `ctx.call_graph()`. So `dispatch_task` in a pipeline pays the full graph-load cost even when prior commands already loaded it. Same issue for `find_test_chunks()` (task.rs:67) which isn't cached in BatchContext at all.
- **Suggested fix:** Either (a) add a `task_with_resources()` variant that accepts pre-loaded graph and test chunks (like `scout_core` does for scout), or (b) have `dispatch_task` call `scout_core` + `gather` + `impact` phases individually using `ctx.call_graph()`, replicating `task()` logic at the batch level. Option (a) is cleaner and mirrors the existing `scout` → `scout_core` pattern.

#### CQ-4: Batch `dispatch_task` token budgeting diverges from CLI waterfall — behavioral inconsistency
- **Difficulty:** medium
- **Location:** `src/cli/batch/handlers.rs:1459-1493` vs `src/cli/commands/task.rs:67-228`
- **Description:** The batch handler applies `--tokens` budget to the code section only (comment at line 1459: "simplified: pack code section only"), while the CLI command uses full 5-section waterfall budgeting (scout 15%, code 50%, impact 15%, placement 10%, notes 10%). This means `cqs batch` with `task "desc" --tokens 500 --json` and `cqs task "desc" --tokens 500 --json` produce materially different output from the same inputs. The batch version retains all scout/risk/test/placement/notes untruncated, only packing code chunks. The `token_count` field in batch JSON response reflects only code tokens used, understating actual output size.
- **Suggested fix:** Extract the waterfall logic from `output_with_budget` into a reusable function (e.g., `waterfall_pack(result, budget, embedder) -> PackedSections`) and call from both CLI and batch paths. The `index_pack` function is already a good building block — the waterfall is just 5 sequential `index_pack` calls with surplus forwarding.

#### CQ-5: `print_code_section_idx` double-iterates content lines
- **Difficulty:** easy
- **Location:** `src/cli/commands/task.rs:554-559`
- **Description:** Lines 554-558 iterate `c.content.lines()` twice: once with `.take(5).collect()` to get the preview lines, then again with `.count()` to check if there are more than 5 lines. For large functions (hundreds of lines), `.count()` re-scans the entire content string just to determine if it exceeds 5. The same pattern exists in `display.rs` (lines 130, 167, 330, 369) but those are pre-existing.
- **Suggested fix:** Count lines during the first iteration: `let lines: Vec<&str> = c.content.lines().collect(); let preview = &lines[..lines.len().min(5)];` and check `lines.len() > 5`. Or use `let sixth = c.content.lines().nth(5)` to check without full iteration.

## Algorithm Correctness

#### AC-1: Waterfall surplus propagation can cause total token usage to exceed stated budget
- **Difficulty:** medium
- **Location:** `src/cli/commands/task.rs:81-195` (`output_with_budget`)
- **Description:** The waterfall token budgeting has a surplus double-counting bug. Each section's budget is computed as `base_percentage + surplus_from_previous`. The notes section (line 184) uses `(budget * 0.10) + remaining` without any surplus cap — it adds its base allocation to the full remaining budget, but that remaining budget already includes the base allocation for notes. Concrete example: budget=100, only scout uses 5 tokens, all other sections except notes are empty. `remaining` stays at 95 through empty middle sections. `notes_budget = 10 + 95 = 105 > 100`. If notes have enough content, total_used = 5 + 105 = 110 > budget. The `token_count` field in JSON output would report a value exceeding `token_budget`.
- **Suggested fix:** Either (a) apply the same `.min(prev_budget.saturating_sub(prev_used))` cap to notes: `notes_budget = (budget * 0.10) + remaining.min(placement_budget.saturating_sub(placement_used))`, or (b) switch to a serial deduction model where each section takes `min(base, remaining)` and updates `remaining -= used`. The latter is simpler and makes overflow impossible by construction.

#### AC-2: Waterfall surplus cascading inflates middle section budgets beyond their documented proportion
- **Difficulty:** medium
- **Location:** `src/cli/commands/task.rs:105-106,116-117,160-161`
- **Description:** Related to AC-1 but affects all sections. The surplus forwarding uses `remaining.min(prev_budget - prev_used)` where `prev_budget` may itself include surplus from earlier sections. When scout uses 0 tokens, `code_budget = 50% + 15% = 65%`. If code also uses 0, `impact_budget = 15% + min(remaining, 65%) = 80%`. The cascading means impact's budget can be up to 80% of total, and placement up to 90%. While individual section budgets can't exceed remaining (AC-1 aside), the distribution doesn't match the documented "scout 15%, code 50%, impact 15%, placement 10%, notes 10%" proportions. Empty early sections cause disproportionate inflation of later sections.
- **Suggested fix:** Same as AC-1 — switch to a serial deduction model. Base percentages serve as priority ordering, not minimum guarantees.

#### AC-3: `index_pack` always includes first item even when budget is 0
- **Difficulty:** easy
- **Location:** `src/cli/commands/task.rs:56`
- **Description:** `index_pack` has the guard `if used + cost > budget && !kept.is_empty()`. When `budget = 0`, the first item always gets packed because `kept.is_empty()` is true on the first iteration. This is documented behavior ("always includes at least one") and tested by `test_index_pack_always_includes_one`. However, with `budget = 0`, the caller explicitly requests zero tokens. Combined with AC-1's surplus cascading, a section budget computed as 0 due to rounding would still emit one item. Low severity — `budget = 0` is unlikely in practice.
- **Suggested fix:** Add `if budget == 0 { return (Vec::new(), 0); }` at the top, or document that budget=0 still returns one item.

#### AC-4: `dedup_tests` deduplicates by name only — same-name tests in different files collapse
- **Difficulty:** easy
- **Location:** `src/task.rs:184-192`
- **Description:** `dedup_tests` uses `seen.insert(t.name.clone())` to deduplicate. If two genuinely different test functions share the same name in different files (e.g., `test_search` in `tests/unit.rs` and `tests/integration.rs`), the second is silently dropped. Rust allows identically-named test functions in different modules. The call graph stores them with the same short name. The test_map and impact systems treat `(name, file)` as identity, but dedup uses only `name`.
- **Suggested fix:** Change the dedup key to `format!("{}:{}", t.name, t.file.display())` to match the identity used elsewhere in the impact system.

#### AC-5: `compute_modify_threshold` makes all tied-top-score chunks ModifyTarget when no clear gap exists
- **Difficulty:** easy
- **Location:** `src/scout.rs:336-337` and `src/scout.rs:347`
- **Description:** When `best_gap < MIN_GAP_RATIO`, `compute_modify_threshold` returns `scores[0]`. `classify_role` uses `score >= modify_threshold`, so all chunks with score == `scores[0]` become `ModifyTarget`. The docstring says "only the top result qualifies" but multiple chunks can share the exact same top score. With RRF scores this is rare, but with pure cosine similarity or when multiple functions have identical NL descriptions, tied scores are plausible. This is more of a documentation inaccuracy than a bug — giving all tied-top-score chunks `ModifyTarget` status is reasonable behavior.
- **Suggested fix:** Update docstring: "only the top result qualifies" -> "only results tied for the top score qualify."

#### AC-6: `compute_modify_threshold` returns 0.0 when all results are test chunks — makes every non-test chunk a ModifyTarget
- **Difficulty:** easy
- **Location:** `src/scout.rs:309-317`
- **Description:** When all search results are test chunks (e.g., searching "test helper" in a test-heavy codebase), the `scores` vec is empty after filtering. The function returns `scores.first().copied().unwrap_or(0.0)` = `0.0`. In `classify_role`, non-test chunks with any positive score satisfy `score >= 0.0`, so all of them become `ModifyTarget`. In practice this is unlikely (scout searches code, not just tests), but if it happens, the role classification is meaningless — every non-test chunk is flagged as a modify target.
- **Suggested fix:** Return `f32::MAX` when scores is empty (or `f32::INFINITY`), so no chunk qualifies as ModifyTarget when there are no non-test results to calibrate against. Alternatively, handle the empty case in `scout_core` before calling `classify_role`.

## Extensibility

#### EX-1: Waterfall budget percentages are magic numbers scattered across function body
- **Difficulty:** easy
- **Location:** `src/cli/commands/task.rs:84,106,117,160,184` and duplicated in tests at `697-711`
- **Description:** The 5-section waterfall allocation (scout 15%, code 50%, impact 15%, placement 10%, notes 10%) is hardcoded as inline `0.15`, `0.50`, etc. at each budget computation site, then duplicated in two tests. Adding a sixth section (e.g., "dependencies") requires modifying 5 budget lines, the surplus-forwarding chain, `PackedSections` struct fields, both output functions (`output_json_budgeted`, `output_text_budgeted`), and both tests. The percentages are also documented in CLAUDE.md, making ~10 places total to keep in sync. Compare with the `index_pack` function which is cleanly reusable — the percentages around it are not.
- **Suggested fix:** Define a `const WATERFALL_WEIGHTS: &[(&str, f64)] = &[("scout", 0.15), ("code", 0.50), ...]` or a small struct, and loop over it. The sections could be driven by a slice of `(name, weight, pack_fn)` tuples, reducing the 5 manual budget blocks to a single loop. Tests would validate `WATERFALL_WEIGHTS.iter().map(|w| w.1).sum::<f64>() == 1.0` against the constant instead of re-stating the values.

#### EX-2: Task BFS gather parameters hardcoded inline
- **Difficulty:** easy
- **Location:** `src/task.rs:103-106`
- **Description:** `task()` hardcodes `.with_expand_depth(2)`, `.with_direction(GatherDirection::Both)`, `.with_max_expanded_nodes(100)` inline when constructing `GatherOptions`. These are tuning knobs that control how aggressively the gather phase expands the call graph. Changing any of these requires editing the function body. `onboard()` has the same pattern (lines 153-156, 167-170) but with different values — there's no central place to see or compare the different profiles. The `limit * 3` truncation at line 110 is another embedded constant.
- **Suggested fix:** Add a `GatherOptions::task_defaults()` constructor (and `onboard_defaults()`) that documents the rationale for the chosen values. Alternatively, add these as fields on a `TaskOptions` struct parallel to `ScoutOptions`, so callers can override without modifying library code.

#### EX-3: ChunkRole serialization to string duplicated in 4 match arms across 3 files
- **Difficulty:** easy
- **Location:** `src/scout.rs:411-413`, `src/cli/commands/task.rs:297-299`, `src/cli/commands/task.rs:515-517`, `src/cli/commands/scout.rs:132-134`
- **Description:** `ChunkRole` has no `Display` or `Serialize` impl. Every JSON serialization site writes its own `match` block. Worse, the string representations differ: `scout_to_json` and `build_scout_json` use `"modify_target"/"test_to_update"/"dependency"`, the text output uses `"modify"/"test"/"dep"`, and `cmd_scout` uses `""/" [test]"/" [dep]"`. Adding a fourth role (e.g., `Caller` or `Reviewer`) requires updating all 4 match arms across 3 files, and there's no compiler-enforced exhaustiveness guarantee since the match arms are inside `serde_json::json!` macros.
- **Suggested fix:** Add `impl Display for ChunkRole` (canonical: `"modify_target"`, etc.) and `impl ChunkRole { fn short_label(&self) -> &str }` for text output (`"modify"`, `"test"`, `"dep"`). JSON serialization uses `.to_string()`, text uses `.short_label()`. `serde::Serialize` derive with `#[serde(rename_all = "snake_case")]` would also work.

#### EX-4: `task()` test depth hardcoded to 5 — not configurable
- **Difficulty:** easy
- **Location:** `src/task.rs:186`
- **Description:** `find_affected_tests_with_chunks(graph, test_chunks, target, 5)` hardcodes the max call-chain depth to 5 for test discovery. The same value appears in `tests/model_eval.rs:572` (though that's just a test). If the codebase grows deeper call chains (common with middleware/decorator patterns), tests won't be discovered beyond depth 5 and there's no way to adjust without editing the library function. The `impact` CLI command exposes this as `--depth` (default 1, clamped to 10), but `task` doesn't forward it.
- **Suggested fix:** Add `test_depth` to a `TaskOptions` struct, defaulting to 5. Or piggyback on the existing `limit` parameter (which is already passed) to scale test depth proportionally.

#### EX-5: Batch dispatch is a single match arm per command — adding a command touches 3 files
- **Difficulty:** easy
- **Location:** `src/cli/batch/commands.rs:24-256` (enum), `src/cli/batch/commands.rs:264-371` (dispatch), `src/cli/batch/handlers.rs` (handler), `src/cli/batch/pipeline.rs:15-17` (PIPEABLE_COMMANDS)
- **Description:** Adding a new batch command requires: (1) add a variant to `BatchCmd` enum, (2) add a match arm in `dispatch()`, (3) write a `dispatch_*` handler function, (4) optionally add to `PIPEABLE_COMMANDS`. This is 3-4 files for every new command. The pattern is mechanical but not enforced — prior audit found `PIPEABLE_COMMANDS` requires manual update (EXT-26, now fixed with a test). The dispatch match is ~110 lines of pure boilerplate routing. This is not terrible for ~25 commands, but the cost is linear with command count.
- **Suggested fix:** This is a pragmatic trade-off: match-arm dispatch is simple and explicit. A registry pattern would add complexity. The current pattern is acceptable as long as the test suite validates completeness (which it does for PIPEABLE_COMMANDS). No immediate fix needed — just acknowledge this is a known cost of the architecture. If command count exceeds ~40, consider a `#[batch_command]` proc macro or dispatch table.

#### EX-6: `compute_modify_threshold` MIN_GAP_RATIO is module-level constant but not adjustable from ScoutOptions
- **Difficulty:** easy
- **Location:** `src/scout.rs:75` (constant), used at `src/scout.rs:336`
- **Description:** `MIN_GAP_RATIO = 0.10` controls the sensitivity of gap detection — the minimum relative score gap required to split ModifyTarget from Dependency chunks. It's a named constant (good), but `ScoutOptions` doesn't expose it. If a user's codebase consistently produces small score gaps (tight clusters) or large gaps (sparse results), they can't tune this without code changes. `ScoutOptions` already has `search_limit` and `search_threshold` — gap ratio is the same category of tuning knob.
- **Suggested fix:** Add `min_gap_ratio: f32` to `ScoutOptions` with `default = 0.10`, pass it into `compute_modify_threshold`. The constant remains as the default value. Low priority — current default works well across tested codebases.

#### EX-7: TaskResult sections are a fixed struct — adding a section requires modifying struct + builder + 3 serializers
- **Difficulty:** medium
- **Location:** `src/task.rs:20-35` (struct), `src/task.rs:155-163` (builder), `src/task.rs:227-322` (`task_to_json`), `src/cli/commands/task.rs:67-228` (waterfall), `src/cli/commands/task.rs:242-276` (JSON budgeted), `src/cli/batch/handlers.rs:1446-1496` (batch)
- **Description:** `TaskResult` is a flat struct with 6 named fields (scout, code, risk, tests, placement, summary). Adding a new section (e.g., "related functions", "dependencies", "affected configs") requires: (1) add field to `TaskResult`, (2) populate in `task()`, (3) serialize in `task_to_json()`, (4) add to waterfall budgeting, (5) add to `output_json_budgeted`, (6) add to `output_text_budgeted`, (7) add to batch handler. That's 7 locations for a new section. The waterfall budgeting is particularly fragile because each section's surplus feeds the next — inserting a section in the middle requires re-wiring the surplus chain. Already flagged tangentially by CQ-2 (JSON duplication) and CQ-4/EH-5 (batch divergence), but this is the structural root cause.
- **Suggested fix:** This is a design trade-off. The flat struct is simple and type-safe — a dynamic `Vec<Section>` would lose type safety. For now, this is acceptable for 6 sections. If sections grow beyond 8-10, consider a `TaskSection` trait with `fn name()`, `fn to_json()`, `fn text_output()`, `fn token_count()` that the waterfall iterates generically.

## Test Coverage

#### TC-1: `task()` has no integration test — only unit tests of sub-functions
- **Difficulty:** medium
- **Location:** `src/task.rs:51-164`
- **Description:** The `task()` function — the main entry point for `cqs task` — has zero integration tests. The inline `#[cfg(test)]` module in `task.rs` tests `extract_modify_targets`, `compute_summary`, `dedup_tests` (inline logic only), and `task_to_json` structure — all with manually constructed inputs. No test ever calls `task()` itself with a real `Store` and `Embedder`. This means the wiring between phases (scout -> gather -> impact -> placement) is untested: if `scout_core` changes its output shape, or `bfs_expand` is called with wrong options, or the `suggest_placement` error path is hit, no test catches it. Compare with `onboard()` which got a full integration test after TC-11 in v0.13.1.
- **Suggested fix:** Add `tests/task_test.rs` integration test: create temp store, index a small fixture, call `task()`, assert non-empty file_groups, code, risk, and summary fields. Pattern: follow `tests/onboard_test.rs`.

#### TC-2: `dedup_tests()` tested via inline HashSet simulation, not the actual function
- **Difficulty:** easy
- **Location:** `src/task.rs:534-571` (test), `src/task.rs:178-193` (function)
- **Description:** The test `test_dedup_tests_removes_duplicates` (line 534) duplicates the HashSet dedup logic inline instead of calling the actual `dedup_tests()` function. The comment says "Can't easily test dedup_tests without a real graph". This means the actual function is never tested — if someone changes the dedup logic (e.g., switches from name-based to ID-based dedup), the test would still pass. The function takes `CallGraph` and `ChunkSummary` slice, which can be constructed from a test Store.
- **Suggested fix:** Either (a) test `dedup_tests()` with a real Store in an integration test, or (b) refactor `dedup_tests` to accept `impl Fn(&str) -> Vec<TestInfo>` so unit tests can inject a mock lookup. Option (a) is simpler if bundled with TC-1.

#### TC-3: `task_to_json` tests check structure but not values for code/risk/tests/placement
- **Difficulty:** easy
- **Location:** `src/task.rs:465-495` (`test_task_to_json_structure`)
- **Description:** `test_task_to_json_structure` asserts that `json["code"]` is an array and `json["risk"]` is an array, but both are empty (the `TaskResult` is constructed with `code: Vec::new(), risk: Vec::new(), tests: Vec::new(), placement: Vec::new()`). No test verifies that `task_to_json` correctly serializes populated code chunks, risk scores, test info, or placement suggestions. The only test with populated data is `test_summary_computation`, which doesn't go through JSON. Since `task_to_json` uses manual `serde_json::json!` construction (not Serialize derives), a field rename or format change would be invisible.
- **Suggested fix:** Add a test that constructs a `TaskResult` with 1+ item in each field (code, risk, tests, placement) and asserts the JSON values match. Specifically test that `risk_level` serialization matches expectations (relates to AD-4 finding: `format!("{:?}", r.risk_level)` produces PascalCase).

#### TC-4: `compute_modify_threshold` untested with all-test-chunk inputs
- **Difficulty:** easy
- **Location:** `src/scout.rs:308-341`
- **Description:** `compute_modify_threshold` filters out test chunks before computing the gap. The existing test `test_compute_modify_threshold_skips_tests` has one test chunk, but no test covers the case where ALL results are tests (e.g., searching "test helper" in a test-heavy codebase). In that case, `scores` is empty and the function returns `0.0`. This flows into `classify_role` where `score >= 0.0` is always true, making every non-test result a ModifyTarget. While unlikely in practice, it would cause misleading output.
- **Suggested fix:** Add a test case where all results have test-like names and verify the function returns 0.0.

#### TC-5: `scout_core()` has no integration test — only tested indirectly via CLI
- **Difficulty:** medium
- **Location:** `src/scout.rs:145-299`
- **Description:** `scout_core()` is the workhorse of both `cqs scout` and `cqs task`. It is `pub(crate)` and has no direct test. The CLI integration tests (`test_scout_json_output`, `test_scout_text_output`) test end-to-end through `cmd_scout` but don't exercise `scout_core` directly. This matters because `scout_core` takes pre-loaded resources (graph, test_chunks) — the shared-resource path used by `task()` — which is different from `scout_with_options()` which loads its own. If the shared-resource path has a bug (e.g., wrong graph), CLI scout tests wouldn't catch it.
- **Suggested fix:** Add a library-level integration test that calls `scout_core()` with pre-loaded graph/test_chunks and verifies the returned `ScoutResult` contains expected file groups, chunk roles, and summary counts. This is more important than testing `scout_with_options` (which is just a wrapper).

#### TC-6: `classify_role` untested at exact threshold boundary with test-like names
- **Difficulty:** easy
- **Location:** `src/scout.rs:344-352`
- **Description:** `classify_role` checks test status first, then threshold. The existing tests cover test detection by name (`test_search`) and file (`tests/integration.rs`), and threshold above/below/at boundary. But no test verifies that a test-like name at or above the modify threshold is still classified as `TestToUpdate`, not `ModifyTarget`. This is the priority of the `if` chain — test detection trumps score. While the current tests imply this (e.g., `test_classify_role_test` uses score 0.9 with threshold 0.5), the intent is not explicit.
- **Suggested fix:** Add an explicit test: `classify_role(0.9, "test_critical", "src/lib.rs", 0.5)` should be `TestToUpdate`, with a comment that test detection takes priority over score.

#### TC-7: Retrieval metrics (`compute_mrr`, `compute_ndcg_at_k`, `compute_recall_at_k`) have no unit tests
- **Difficulty:** medium
- **Location:** `tests/model_eval.rs:1244-1407`
- **Description:** The three retrieval metric functions are only exercised indirectly by the `#[ignore]` slow model eval tests which download ONNX models. No fast unit test verifies their mathematical correctness with known inputs. `compute_mrr` should return 1.0 when the expected result is always rank 1, 0.5 when always rank 2, etc. `compute_ndcg_at_k` with rank 1 should return 1.0 (since 1/log2(2) = 1.0). `compute_recall_at_k` with k=1 should return (hits, total) matching rank-1 presence. These are standalone pure functions that take `&[IndexedChunk]`, `&[EvalCase]`, and `&[Vec<f32>]` — easily testable without any model.
- **Suggested fix:** Add fast `#[test]` (not `#[ignore]`) tests in `model_eval.rs`: construct 3-4 `IndexedChunk` with known embeddings (unit vectors along axes), 2-3 `EvalCase` with known expected names, and hand-computed query embeddings. Assert exact MRR, NDCG, and recall values. Example: if query = [1,0,0] and expected is chunk with embedding [1,0,0], rank is 1, MRR = 1.0, NDCG@k = 1.0, recall@k = 1/1.

#### TC-8: `index_pack` untested with zero budget
- **Difficulty:** easy
- **Location:** `src/cli/commands/task.rs:36-64`
- **Description:** `index_pack` has a special case: when `budget` is 0 and items exist, the `if used + cost > budget && !kept.is_empty()` guard lets the first item through (because `kept.is_empty()` is true). So `index_pack(&[100], 0, 0, |_| 1.0)` returns `([0], 100)` — the first item is always included regardless of budget. This is the same behavior tested by `test_index_pack_always_includes_one`, but that test uses budget=10 with item cost 100. No test uses budget=0 explicitly. While the "always include one" behavior is tested, the zero-budget edge case where `0 + cost > 0` triggers immediately could interact differently with surplus forwarding in the waterfall.
- **Suggested fix:** Add `index_pack(&[50, 50], 0, 0, |_| 1.0)` and assert it returns `([0], 50)` — only the first item despite budget=0.

#### TC-9: No CLI integration test for `cqs task`
- **Difficulty:** medium
- **Location:** `src/cli/commands/task.rs:8-32` (`cmd_task`)
- **Description:** `tests/cli_commands_test.rs` has integration tests for scout, where, related, impact-diff, stale, query, gather, and ref commands. There is no integration test for `cqs task`. The batch test file (`cli_batch_test.rs`) also has no `task` command test. This means the full pipeline (CLI argument parsing -> `cmd_task` -> `task()` -> JSON output) is completely untested at the integration level. Token budgeting output (`output_with_budget`), text formatting (`output_text`), and the batch `dispatch_task` handler are all untested.
- **Suggested fix:** Add `test_task_json_output()` and `test_task_text_output()` to `cli_commands_test.rs`, following the pattern of `test_scout_json_output`. Add `test_batch_task()` to `cli_batch_test.rs`. These catch wiring bugs between CLI and library.

#### TC-10: `note_mention_matches_file` untested with empty strings
- **Difficulty:** easy
- **Location:** `src/scout.rs:383-391`
- **Description:** No test covers `note_mention_matches_file("", "src/foo.rs")` or `note_mention_matches_file("foo.rs", "")`. The empty-mention case would return `false` (no '.' or '/'), which is correct. The empty-file case: `"".ends_with("foo.rs")` is `false`, also correct. But the boundary check `file.as_bytes()[file.len() - mention.len() - 1]` would underflow if `file.len() < mention.len() + 1` — however `ends_with` guards this. Still, the edge case is worth a test to document behavior.
- **Suggested fix:** Add assertions for empty mention and empty file to the existing `test_note_mention_matches_file` test.

#### TC-11: Waterfall surplus forwarding logic untested
- **Difficulty:** medium
- **Location:** `src/cli/commands/task.rs:84-184`
- **Description:** The waterfall budget logic forwards surplus from one section to the next: e.g., if scout uses 50 of its 150-token budget, the remaining 100 flows to the code section. This surplus forwarding depends on `remaining.min(scout_budget.saturating_sub(scout_used))` and similar expressions. No test exercises this path. The existing `test_waterfall_allocation_percentages` and `test_waterfall_section_budgets` verify that percentages sum to 1.0 and integer budgets are correct, but they don't test the actual surplus forwarding behavior (which requires a real `TaskResult` with varying section sizes and an `Embedder` for token counting).
- **Suggested fix:** This requires either (a) an integration test with real embedder, or (b) extracting the surplus calculation into a pure function testable without embedder. Option (b) is cleaner: extract `compute_section_budgets(budget: usize, section_tokens: [usize; 5]) -> [usize; 5]` and test it with known inputs where early sections are under/over budget.

## Platform Behavior

**Result:** v0.14.0 code is largely clean on platform behavior. Existing infrastructure (`normalize_origin`, `rel_display`, `note_mention_matches_file` backslash normalization) handles WSL/Windows path issues consistently. The new task/scout code delegates to these functions correctly. Two minor findings.

#### PB-1: `is_test_chunk` path patterns use forward-slash only — fails on native Windows paths
- **Difficulty:** easy
- **Location:** `src/lib.rs:201-207`
- **Description:** `is_test_chunk()` checks `file.contains("/tests/")`, `file.starts_with("tests/")`, etc. — all with forward-slash separators. In the v0.14.0 scout code, this is called at `src/scout.rs:311` and `src/scout.rs:345` with `chunk.file.to_string_lossy()` as the file argument. Since chunk files come from the DB (stored with forward slashes via `normalize_origin`), this works correctly on WSL/Linux. However, `is_test_chunk` is a public function also called from `src/store/calls.rs:812` with `path_str` from `to_string_lossy()` on a `PathBuf` constructed from filesystem walks — if cqs were ever built natively on Windows, those paths would have backslashes and test detection would silently fail. The scout code itself is safe because it reads from DB, but it inherits this latent fragility from the shared `is_test_chunk` function.
- **Suggested fix:** Add backslash variants to the path checks: `file.contains("/tests/") || file.contains("\\tests\\")`, or normalize the file argument with `.replace('\\', "/")` before matching. Low priority — cqs targets WSL/Linux where this isn't triggered.

#### PB-2: Scout staleness check mixes `to_str()` and `to_string_lossy()` for the same PathBuf keys
- **Difficulty:** easy
- **Location:** `src/scout.rs:202` and `src/scout.rs:223`
- **Description:** Line 202 converts `file_map` keys with `p.to_str().unwrap_or("")` to pass to `check_origins_stale()`. Line 223 converts the same keys with `file.to_string_lossy().to_string()` to check membership in `stale_set`. For non-UTF-8 paths, `to_str()` returns `None` (mapped to `""`) while `to_string_lossy()` returns a string with U+FFFD replacement characters. This means a non-UTF-8 path would be queried as `""` in the staleness check (finding nothing in the DB), but tested for membership as a `\u{FFFD}`-containing string (never matching the DB results). The path would be marked not-stale regardless of actual file modification time. EH-4 already covers the `unwrap_or("")` half of this — this finding adds the cross-function inconsistency: even if EH-4 is fixed with `filter_map`, the `to_string_lossy` on line 223 should also be updated to match.
- **Suggested fix:** Use the same conversion on both lines. After applying EH-4's fix (`.filter_map(|p| p.to_str())`), also change line 223 to use `to_str()` with the same filter, or better, store the string representation in `file_map` keys (use `HashMap<String, ...>` instead of `HashMap<PathBuf, ...>`) so the same string is used for both staleness checking and set membership.

## Performance

#### PF-1: `task()` placement phase re-embeds the query — redundant ONNX inference
- **Difficulty:** medium
- **Location:** `src/task.rs:136` calling `suggest_placement()`, which calls `embedder.embed_query(description)` at `src/where_to_add.rs:115-117`
- **Description:** `task()` embeds the query at line 61 (`embedder.embed_query(description)`) and passes the embedding to `scout_core`. But the placement phase at line 136 calls `suggest_placement(store, embedder, description, 3)`, which internally calls `embedder.embed_query(description)` again. ONNX inference is the most expensive single operation in a `task()` call (~50-100ms on GPU, ~200-500ms on CPU). The same description string is embedded twice, and both calls also perform a full `search_filtered()` against the index — so the HNSW search is also doubled. The scout phase search and the placement search use identical parameters (both `enable_rrf: true`, both use the full query text), differing only in `search_limit` (scout uses `opts.search_limit` = 15, placement uses its own default = 10).
- **Suggested fix:** Add a `suggest_placement_with_embedding()` variant (or extend `suggest_placement_with_options`) that accepts a pre-computed embedding and optionally pre-filtered search results. `task()` can then pass the embedding from line 61 and avoid the second ONNX call. If scout's search results are a superset (limit 15 >= 10), the placement phase could reuse them entirely, eliminating both the embedding and the HNSW search.

#### PF-2: `task()` duplicates reverse BFS across impact and test discovery — same graph traversal done twice per target
- **Difficulty:** medium
- **Location:** `src/task.rs:124` (`compute_risk_batch`) and `src/task.rs:132` (`dedup_tests` → `find_affected_tests_with_chunks`)
- **Description:** For each modify target, `task()` performs the identical `reverse_bfs(graph, target_name, depth)` twice: once inside `compute_risk_batch` (via `compute_hints_with_graph` → `reverse_bfs`) and once inside `dedup_tests` (via `find_affected_tests_with_chunks` → `reverse_bfs`). Both traverse the same call graph from the same starting nodes. With `N` modify targets, this is `2N` BFS traversals instead of `N`. Each BFS walks the reverse call graph up to depth 5 (the default `DEFAULT_MAX_TEST_SEARCH_DEPTH`), visiting hundreds of nodes in a typical codebase. The `reverse_bfs` result (ancestor HashMap) contains all the information both consumers need: `compute_hints_with_graph` counts test chunks in ancestors, and `find_affected_tests_with_chunks` extracts test info from ancestors.
- **Suggested fix:** Factor out a shared `compute_impact_with_tests()` that calls `reverse_bfs` once per target and returns both the `RiskScore` and the `Vec<TestInfo>`. Alternatively, cache the BFS results in a HashMap keyed by target name, since the targets are the same in both calls.

#### PF-3: `scout_core` calls `compute_hints_with_graph` per chunk — each does `reverse_bfs` up to depth 3
- **Difficulty:** medium
- **Location:** `src/scout.rs:228-233` (inside the file group building loop)
- **Description:** For each search result chunk (up to 15 by default), `scout_core` calls `compute_hints_with_graph()` which calls `reverse_bfs(graph, name, DEFAULT_MAX_TEST_SEARCH_DEPTH)`. The default test search depth is 3. Each BFS explores the reverse call graph from that function, visiting all ancestors up to depth 3 and then scanning all test chunks against the ancestor set. With 15 search results, that's 15 independent BFS traversals. The BFS results overlap significantly when multiple search results are in the same call subgraph (e.g., `foo()` calls `bar()` calls `baz()` — all 3 results share much of their ancestor trees). A multi-source BFS (`reverse_bfs_multi`, which already exists at `src/impact/bfs.rs:40`) would traverse shared ancestors once instead of per-source.
- **Suggested fix:** Use `reverse_bfs_multi` from `src/impact/bfs.rs:40` for all chunks at once, then derive per-chunk caller/test counts from the combined result. This requires tracking which ancestors belong to which source, which the current `reverse_bfs_multi` doesn't do — but even a simple cache of "already visited" nodes across calls would help. Alternatively, accept the per-chunk BFS as a pragmatic trade-off since `N=15` is small and BFS depth is 3.

#### PF-4: Waterfall budgeting clones all code content strings unnecessarily
- **Difficulty:** easy
- **Location:** `src/cli/commands/task.rs:107`
- **Description:** `let code_texts: Vec<String> = result.code.iter().map(|c| c.content.clone()).collect()` clones every `GatheredChunk.content` string into a new `Vec<String>`, then immediately takes `&str` references from those clones for `count_tokens_batch`. The `content` field is already a `String` on `GatheredChunk` — `c.content.as_str()` provides a `&str` directly. Code chunks can be hundreds of lines, so cloning all of them allocates significant memory (total content size can be 10-100KB). The same pattern repeats for `group_texts` (line 85-96) where it's necessary because the strings are constructed via `join()`, but for `code_texts` it's pure waste.
- **Suggested fix:** Replace with `let code_text_refs: Vec<&str> = result.code.iter().map(|c| c.content.as_str()).collect()` and remove the intermediate `Vec<String>`. Pass `&code_text_refs` directly to `count_tokens_batch`.

#### PF-5: `find_relevant_notes` is O(N*M*F) — notes * mentions * result_files
- **Difficulty:** easy
- **Location:** `src/scout.rs:368-375`
- **Description:** `find_relevant_notes` iterates all notes, and for each note iterates all mentions, and for each mention iterates all result files calling `note_mention_matches_file`. With `N` notes, `M` average mentions per note, and `F` result files, this is O(N*M*F). The `note_mention_matches_file` function does two `String::replace('\\', "/")` allocations per call. In typical use, `N` < 50 notes, `M` < 3 mentions, `F` < 10 files, so total iterations < 1500 — well within acceptable bounds. However, the backslash replacement allocates on every call even on Linux where backslashes never appear.
- **Suggested fix:** Low priority. The scale is acceptable for typical codebase sizes. If it ever becomes a concern: pre-normalize the result_files set and mentions once before the loop, and use a suffix trie or reverse index for O(1) lookups. For now, document the O(N*M*F) complexity.

#### PF-6: `task_to_json` constructs full JSON then batch handler overwrites `code` key — wasted serialization
- **Difficulty:** easy
- **Location:** `src/cli/batch/handlers.rs:1457` and `src/cli/batch/handlers.rs:1490`
- **Description:** In `dispatch_task`, when `--tokens` is specified, the handler first calls `cqs::task_to_json(&result, &ctx.root)` at line 1457 which serializes ALL code chunks into JSON. Then at line 1490, `json["code"] = serde_json::json!(code_json)` overwrites the code array with the token-budgeted subset. The original serialization of all code chunks (which can be the largest section by far — hundreds of lines of content per chunk) is thrown away. The code content strings are serialized to `serde_json::Value` then immediately discarded.
- **Suggested fix:** When `tokens` is `Some`, build the JSON manually instead of calling `task_to_json` first. Or add a `task_to_json_without_code()` variant. Since CQ-3 already covers the broader `dispatch_task` architecture issue, this is a subset concern — fixing CQ-3 would likely fix this too.

## Red Team

### Target 1: Batch Pipeline Bypass

#### RT-INJ-1: Pipeline boundary is clean — no bypass via shell-words quoting
- **Severity:** n/a (no finding)
- **Location:** `src/cli/batch/mod.rs:293-310`, `src/cli/batch/pipeline.rs:101-114,312`
- **Attack vector:** Batch input containing `|` inside quoted strings or embedded in tokens.
- **PoC:** `shell_words::split('search "error|handling"')` produces `["search", "error|handling"]`. `has_pipe_token` (line 312) checks for standalone `"|"` tokens only. Embedded `|` inside tokens does not trigger pipeline mode. `split_tokens_by_pipe` splits on exact `"|"` match. An attacker cannot inject a pipeline stage through quoted input — the pipe must be a standalone whitespace-separated token. All pipeline stages after stage 0 are validated against `PIPEABLE_COMMANDS` whitelist (line 137-146), and quit/exit/help are explicitly blocked (line 149-158).
- **Impact:** None — stated protection is correct.
- **Suggested mitigation:** None needed.

### Target 2: FTS5 Bypass

#### RT-INJ-2: All FTS5 MATCH code paths are sanitized — no bypass found
- **Severity:** n/a (no finding)
- **Location:** `src/search.rs:535-536`, `src/store/mod.rs:559,591`, `src/store/chunks.rs:1064`, `src/store/notes.rs:52`
- **Attack vector:** Searched for any code path reaching `chunks_fts MATCH` or `notes_fts MATCH` that bypasses `sanitize_fts_query(normalize_for_fts(...))`. Found 5 MATCH callsites, all sanitized:
  1. `search_filtered` (search.rs:548): sanitized at line 535-536.
  2. `search_fts` (store/mod.rs:570): sanitized at line 559.
  3. `search_by_name` (store/mod.rs:605): sanitized at line 591.
  4. `batch_search_by_name` (store/chunks.rs:1084): sanitized at line 1064.
  5. `insert_note_with_fts` (store/notes.rs:52): INSERT path (not MATCH), FTS injection n/a.

  New v0.14.0 code paths (`scout_core` at scout.rs:162, `task()` via scout_core) reach FTS only through `store.search_filtered()`, which sanitizes internally.
- **Impact:** None.
- **Suggested mitigation:** None needed.

### Target 3: TOML Injection

#### RT-INJ-3: TOML serialization correctly handles metacharacters — no injection possible
- **Severity:** n/a (no finding)
- **Location:** `src/note.rs:237-245` (`rewrite_notes_file`)
- **Attack vector:** Note text containing TOML metacharacters: `"""`, `[[note]]`, `\n[note]\nsentiment = 1`.
- **PoC:** `cmd_notes_add` creates a `NoteEntry { text: text.to_string(), ... }`. `rewrite_notes_file` calls `toml::to_string_pretty(&file)` via serde `Serialize` derive on `NoteEntry`/`NoteFile`. The `toml` crate serializer properly escapes all metacharacters: strings containing control characters, `"`, `\`, newlines use basic string escaping. Triple-quote `"""` in note text is serialized as `"\"\"\"" ` (each quote escaped). The fuzz tests at note.rs:496-533 (proptest) confirm `parse_notes_str` never panics on arbitrary input, and the round-trip test `test_rewrite_update_note` verifies serialization fidelity. Note text is capped at 2000 bytes (cli/commands/notes.rs:168).
- **Impact:** None — `toml::to_string_pretty` is a safe serializer.
- **Suggested mitigation:** None needed.

### Target 4: Ref Name Injection

#### RT-INJ-4: `validate_ref_name` does not reject null bytes
- **Severity:** low
- **Location:** `src/reference.rs:209-220`
- **Attack vector:** `cqs ref add "valid\x00evil" /path/to/source`
- **PoC:**
  1. `validate_ref_name("valid\x00evil")` — passes all checks (no `/`, `\`, `..`, `.`).
  2. `ref_path` returns `~/.local/share/cqs/refs/valid\x00evil`.
  3. `std::fs::create_dir_all` calls `libc::mkdir` via `CString::new()`, which returns `Err(NulError)` for null-containing paths. The directory creation fails.
  4. However, the error message is confusing ("nul byte found in string"), and the name may have already been checked against config entries (cli/commands/reference.rs:74) before the filesystem operation fails.
  5. Practical exploitability is very low: the kernel's `execve` uses C strings, so CLI arguments cannot contain null bytes. Only programmatic callers (batch mode or library) could craft this input. Batch mode's `gather --ref "name"` passes through `shell_words::split` which preserves null bytes in Rust strings, but the config lookup at `get_ref` would fail with "not found" since no config entry has a null-byte name.
- **Impact:** Confusing error message. No filesystem or data impact.
- **Suggested mitigation:** Add `name.contains('\0')` to `validate_ref_name` for defense-in-depth.

#### RT-INJ-5: Batch `get_ref` does not call `validate_ref_name` — but ref name is a config lookup key, not a filesystem path
- **Severity:** n/a (no finding after analysis)
- **Location:** `src/cli/batch/mod.rs:91-122`
- **Attack vector:** `gather "query" --ref "../../../etc"` in batch stdin.
- **PoC:** `ctx.get_ref("../../../etc")` filters `config.references` by `r.name == name`. Since reference names in config are written by `cqs ref add` (which validates via `ref_path` -> `validate_ref_name`), no config entry can have traversal characters. The ref name is never used as a filesystem path in the batch path — `load_references` uses the config's `path` field, not the name. Config files are trusted per the threat model.
- **Impact:** None.
- **Suggested mitigation:** None needed.

### Target 5: shell-words Edge Cases

#### RT-INJ-6: shell-words edge cases handled correctly — no bypass
- **Severity:** n/a (no finding)
- **Location:** `src/cli/batch/mod.rs:293-302`
- **Attack vector:** Unbalanced quotes, null bytes, extremely long tokens.
- **PoC:** `shell_words::split("search \"unterminated")` returns `Err(...)`, caught at line 296, error logged. Null bytes in Rust strings are valid UTF-8 and pass through shell-words to clap, then to parameterized SQL (safe) or embedding model (safe). The 1MB line limit (MAX_BATCH_LINE_LEN at line 33) bounds token sizes. `shell_words::split` is O(n) — no ReDoS.
- **Impact:** None.
- **Suggested mitigation:** None needed.

### Target 6: CQS_PDF_SCRIPT

#### RT-INJ-7: `CQS_PDF_SCRIPT` script path not validated beyond existence check — prior SEC-8 extension check not implemented
- **Severity:** low (documented attack surface)
- **Location:** `src/convert/pdf.rs:54-63`
- **Attack vector:** `CQS_PDF_SCRIPT=/tmp/malicious.sh cqs convert document.pdf`
- **PoC:**
  1. `find_pdf_script()` reads env var at line 56.
  2. Line 57: `tracing::warn!` logs the path (SEC-8 fix from v0.12.4).
  3. Lines 58-60: Only checks `p.exists()`. No extension validation, no canonicalization.
  4. `pdf_to_markdown` passes the script to `Command::new(&python).args([&script, ...])` (line 21).
  5. The python interpreter receives the script path as an argument and executes it.
  6. A non-Python script (e.g., `/tmp/malicious.sh`) would cause Python to fail with a syntax error — Python cannot execute shell scripts. So the attack is limited to Python scripts.
  7. However: if `find_python()` returned a symlink to a different interpreter, the constraint changes. `find_python` (line 88) tries `python3` then `python` from `$PATH` — both under user control (environment is trusted).

  **Gap from prior audit:** SEC-8 recommended "verify `.py` extension" — only the warning was implemented, not the extension check.
- **Impact:** Arbitrary Python code execution, but only when the user's environment is controlled by the attacker (trusted boundary).
- **Suggested mitigation:** Add a soft warning (not a block) for non-`.py` extensions, completing the SEC-8 recommendation: `if !p.extension().is_some_and(|e| e == "py") { tracing::warn!("CQS_PDF_SCRIPT does not have .py extension — ensure this is intentional"); }`.

### Target 7: Glob ReDoS

#### RT-INJ-8: `globset` is not vulnerable to ReDoS — bounded by design
- **Severity:** n/a (no finding)
- **Location:** `src/search.rs:235-243`
- **Attack vector:** Pathological glob patterns via `--path`.
- **PoC:** The `globset` crate converts glob patterns into DFA or bounded NFA using `regex-automata`, which has explicit ReDoS protections (DFA state limits, bounded NFA simulation). Invalid patterns are rejected at `Glob::new()` time, caught by `compile_glob_filter` which logs a warning and returns `None`. Confirmed by test at search.rs:1005-1008.
- **Impact:** None.
- **Suggested mitigation:** None needed.

### Additional Finding

#### RT-INJ-9: `cmd_ref_add` validates name late — confusing error on invalid ref names
- **Severity:** low
- **Location:** `src/cli/commands/reference.rs:64-89`
- **Attack vector:** `cqs ref add "foo/bar" /path/to/source`
- **PoC:**
  1. `cmd_ref_add` receives name `"foo/bar"` (line 64).
  2. Lines 74-80: Duplicate check against config — passes (no existing "foo/bar").
  3. Lines 83-84: Source path canonicalization — succeeds.
  4. Line 87: `reference::ref_path("foo/bar")` calls `validate_ref_name("foo/bar")` which returns `Err` (contains `/`). `ref_path` returns `None`.
  5. Line 88: `ok_or_else` produces error "Could not determine reference storage directory".

  The error message is misleading — it says "could not determine storage directory" when the actual problem is the invalid ref name. The user doesn't know what's wrong.
- **Impact:** UX issue only — no security impact. The name IS rejected, just with a confusing error.
- **Suggested mitigation:** Call `reference::validate_ref_name(name)?` at the top of `cmd_ref_add` (before duplicate check) to fail fast with a clear error message like "Reference name cannot contain '/', '\\', or '..'".

### Target 8: Filesystem Boundary — Path Traversal via `cqs read`

#### RT-FS-1: `validate_and_read_file` canonicalize+starts_with is applied on ALL file-reading code paths — no bypass found
- **Severity:** n/a (no finding)
- **Location:** `src/cli/commands/read.rs:27-56`
- **Attack vector:** Attempted to find any code path that reaches `std::fs::read_to_string` with user-controlled path input without passing through the `canonicalize+starts_with` check.
- **PoC:** Exhaustive check of all `std::fs::read_to_string` and `std::fs::read` callsites:
  1. **`cqs read <path>`** (cli/commands/read.rs:273): calls `validate_and_read_file(&root, path)` — protected.
  2. **Batch `read <path>`** (cli/batch/handlers.rs:1238): calls `validate_and_read_file(&ctx.root, path)` — protected.
  3. **`cqs read --focus <name>`** (cli/commands/read.rs:306): calls `build_focused_output` which reads from the database via `resolve_target(store, focus)`. No filesystem read of the target — content comes from stored chunks. Not a file read path.
  4. **Batch `read --focus <name>`** (cli/batch/handlers.rs:1261): same as above, calls `build_focused_output`. No filesystem read.
  5. **`display.rs:read_context_lines`** (cli/display.rs:34): reads from `root.join(&r.chunk.file)` where `chunk.file` comes from the local DB. The DB is the user's own index — trusted per threat model. Reference results are explicitly excluded from context line reading (display.rs:312,340: `if tagged.source.is_none()`).
  6. **`query.rs` parent context** (cli/commands/query.rs:688): reads from `root.join(&sr.chunk.file)` — same as display.rs, DB-sourced paths from trusted local index.
  7. **Indexing paths** (source/filesystem.rs:108, parser/mod.rs:153, parser/calls.rs:225): only called during `cqs index`/`cqs watch`, reads user's own project files. Trusted operation.
  8. **Convert paths** (convert/mod.rs:113, convert/html.rs:32, convert/chm.rs:122, convert/webhelp.rs:96): reads source documents during explicit user-initiated conversion. CHM extraction has zip-slip containment (chm.rs:54-82).
  9. **Config/notes/audit** (config.rs, note.rs, audit.rs): reads well-known config files at fixed paths. Not user-path-controlled.
- **Impact:** None. The stated protection covers all user-facing file read paths.
- **Suggested mitigation:** None needed.

### Target 9: Filesystem Boundary — Convert Output Escape

#### RT-FS-2: Convert `--output` writes to arbitrary directories — documented and accepted
- **Severity:** n/a (documented behavior)
- **Location:** `src/cli/commands/convert.rs:25-26`, `src/convert/mod.rs:230`
- **Attack vector:** `cqs convert doc.pdf --output /tmp/evil/`
- **PoC:** SECURITY.md explicitly states: "The output path is not sandboxed beyond normal filesystem permissions." The convert command takes `--output` as a `PathBuf` (convert.rs:25-26) and passes it directly to `ConvertOptions.output_dir` (convert.rs:44). The `finalize_output` function (convert/mod.rs:230) calls `create_dir_all(&opts.output_dir)` and writes the converted file there. No check restricts the output to the project root.
- **Impact:** The user can write converted Markdown files to any directory they have write access to. This is by design — convert is a utility command, not a sandboxed operation.
- **Suggested mitigation:** None — this is documented behavior. The user is trusted.

### Target 10: Filesystem Boundary — Reference Index Path Traversal

#### RT-FS-3: Reference name validation blocks traversal — defense in depth with canonicalize
- **Severity:** n/a (no finding)
- **Location:** `src/reference.rs:208-226`, `src/cli/commands/reference.rs:205-235`
- **Attack vector:** `cqs ref add "../../../tmp/evil" /path/to/source`, `cqs ref remove "../../../tmp/evil"`
- **PoC:**
  1. **`ref add`**: `ref_path("../../../tmp/evil")` calls `validate_ref_name`, which rejects the name at line 213 (`name.contains("..")`). Returns `None`. `cmd_ref_add` fails at line 88 with "Could not determine reference storage directory". The directory is never created.
  2. **`ref remove`**: `cmd_ref_remove("../../../tmp/evil")` first calls `remove_reference_from_config`, which looks up `name` in the TOML config. Since `ref add` never stored a `../`-containing name, `remove` returns `false` and bails at line 211. Even if a config entry somehow existed: the deletion path (lines 216-235) constructs `refs_root.join(name)`, then applies `canonicalize+starts_with` at lines 220-224 before `remove_dir_all`. A traversal path that escapes refs_root would be caught and refused with a warning.
  3. **Null byte in name**: `validate_ref_name("valid\x00evil")` passes validation (RT-INJ-4, separate finding), but `create_dir_all` fails because `CString::new()` rejects null bytes. No filesystem impact.
  4. **Batch `--ref` parameter**: batch's `get_ref` does a config name lookup, never constructs filesystem paths from the ref name (covered by RT-INJ-5).
- **Impact:** None. Both name validation and canonicalize+starts_with provide layered defense.
- **Suggested mitigation:** None needed.

### Target 11: Filesystem Boundary — Function Name as Path Component

#### RT-FS-4: Function names are never used as filesystem path components — no path traversal possible
- **Severity:** n/a (no finding)
- **Location:** `src/search.rs:57-84` (`resolve_target`), `src/cli/commands/read.rs:115-258` (`build_focused_output`)
- **Attack vector:** `cqs read --focus "../../etc/passwd"`, `cqs explain "../../etc/passwd"`
- **PoC:**
  1. `resolve_target(store, "../../etc/passwd")` at search.rs:57: parses the target string at line 58, then calls `store.search_by_name(name, 20)` at line 59. This is a parameterized SQL query (`WHERE chunks_fts MATCH ?`). The name is never used as a path component — it's a database lookup key.
  2. If no matching function exists (likely for "../../etc/passwd"), `resolve_target` returns `Err(StoreError::Runtime("No function found matching..."))`. The command fails safely.
  3. `build_focused_output` reads all content from the store's `ChunkSummary` (line 195: `chunk.content`), not from disk. Even if a function named `../../etc/passwd` existed in the index, its content would be the indexed source code, not the actual `/etc/passwd` file.
  4. `cqs explain` follows the same path through `resolve_target`.
- **Impact:** None. Function names are database lookup keys, never filesystem paths.
- **Suggested mitigation:** None needed.

### Target 12: Filesystem Boundary — Stale Index Entries as Indirect Reads

#### RT-FS-5: Stale index entries serve stored content, not current file content — no indirect read
- **Severity:** n/a (no finding)
- **Location:** `src/cli/commands/read.rs:27-56` (`validate_and_read_file`), `src/cli/commands/read.rs:115-258` (`build_focused_output`)
- **Attack vector:** Index a file, then replace it with a symlink to `/etc/passwd`. Does `cqs read --focus <function>` serve the symlink target?
- **PoC:**
  1. **`cqs read <path>`**: Uses `validate_and_read_file`, which reads from disk. `dunce::canonicalize` resolves symlinks. If the canonical path is outside the project root, the traversal check fails. If a file was replaced by a symlink pointing to `/etc/passwd`, canonicalization resolves to `/etc/passwd`, `starts_with(project_root)` fails. Content is NOT served.
  2. **`cqs read --focus <function>`**: Uses `build_focused_output`, which reads content from `chunk.content` (database). The content is what was indexed at index time — the original file content. Even if the file has been replaced, the stored content is stale but safe (it's the original code, not the new target). No disk read occurs.
  3. **`display.rs` context lines**: Reads from disk via `root.join(&chunk.file)`, where `chunk.file` is the stored origin path. If the file was replaced by a symlink, the read might follow it — but this only produces context lines for display (before/after the chunk), not chunk content itself. The file path comes from the user's own DB (trusted). This is a display-only path, not a data extraction path. Note: the TOCTOU documented in SECURITY.md applies here.
- **Impact:** None. The `cqs read` path validates. The `--focus` path reads from DB. Context lines are display-only from trusted DB paths.
- **Suggested mitigation:** None needed.

### Adversarial Robustness (RT-RES)

#### RT-RES-1: Pipeline intermediate merge has no fan-out cap — unbounded name extraction before truncation
- **Severity:** medium
- **Location:** `src/cli/batch/pipeline.rs:260-275` (intermediate stage merge)
- **Attack vector:** `search "common term" | callers | callers | callers`
- **PoC:** Stage 0 returns 15 results. Stage 1 fans out to 50 `callers` calls (capped by `PIPELINE_FAN_OUT_LIMIT=50`). Each returns ~10 callers. The intermediate merge at lines 260-268 collects ALL unique names from ALL 50 dispatch results before applying the fan-out cap. `extract_names` is called on each of 50 results, yielding up to 500 unique names stored in `merged_names` and `merged_seen` (HashSet). These 500 names are wrapped into a synthetic JSON array of 500 objects at line 271-275. The *next* stage's `extract_names` then processes this, and the fan-out cap at line 198 truncates to 50 — but the intermediate data structure held 500 entries. With a 4-stage pipeline: stage results grow to `50 * avg_result_size` per stage, all held in memory before the merge+truncate. For `explain` (which returns full function cards with content), 50 results could be 1-5MB of JSON held simultaneously.
- **Impact:** Memory spike proportional to `PIPELINE_FAN_OUT_LIMIT * per_call_result_size` per stage. Not unbounded (capped at 50 dispatches), but multi-stage pipelines accumulate. No crash — just higher memory usage than necessary.
- **Suggested mitigation:** Apply `PIPELINE_FAN_OUT_LIMIT` to the intermediate merge's `merged_names` as well: `if merged_names.len() >= PIPELINE_FAN_OUT_LIMIT { break; }` in the extract loop. Prevents collecting names that will be truncated anyway.

#### RT-RES-2: `gather` BFS correctly bounded — cycles and fan-out both handled
- **Severity:** none (verified safe)
- **Location:** `src/gather.rs:143-192` (`bfs_expand`)
- **Attack vector:** `cqs batch` with `gather "hub function" --expand 10`
- **PoC:** `bfs_expand` uses `name_scores` HashMap as visited set. `Entry::Vacant` at line 176 ensures nodes are only queued once (or updated if a higher score is found, but not re-queued at line 183). `max_expanded_nodes` (default 200) caps total nodes at lines 162 and 171. Depth limit at line 159. Cycles in the call graph are handled: if A calls B and B calls A, A is inserted first (depth 0), B is found as callee (depth 1), A is already in the map (Occupied entry), so B's neighbor A is skipped. BFS terminates. **Verified safe across all traversal modes (callers, callees, both).**
- **Impact:** None — correctly bounded.
- **Suggested mitigation:** None needed. Consider clamping user-supplied `expand_depth` to max 10 in `dispatch_gather` for defense in depth.

#### RT-RES-3: `--tokens 0` causes `index_pack` to emit one item per section despite zero budget
- **Severity:** low
- **Location:** `src/cli/commands/task.rs:56` (`index_pack` "always include one" guard)
- **Attack vector:** `cqs task "anything" --tokens 0 --json`
- **PoC:** With `budget = 0`: each section's budget computes to 0 (`(0 * 0.15) as usize = 0`). `index_pack` with budget=0 and non-empty items: first item passes because `!kept.is_empty()` is false on the first iteration (line 56). Each of the 5 waterfall sections packs exactly 1 item. `total_used` sums to whatever those items cost (could be 200+ tokens). JSON output: `{"token_count": 200, "token_budget": 0}` — token_count exceeds stated budget. No crash or panic.
- **Impact:** Misleading JSON output where `token_count > token_budget`. Consumers trusting the budget constraint get more data than expected.
- **Suggested mitigation:** Add `if budget == 0 { return (Vec::new(), 0); }` at top of `index_pack`. Or clamp `max_tokens` to minimum 1 at CLI entry.

#### RT-RES-4: `--tokens` with extreme values — f64 precision loss is cosmetic only
- **Severity:** none (verified safe)
- **Location:** `src/cli/commands/task.rs:84`
- **Attack vector:** `cqs task "anything" --tokens 18446744073709551615 --json`
- **PoC:** `budget as f64 * 0.15`: `usize::MAX` as f64 rounds to `1.8446744073709552e19`. `* 0.15 = 2.767e18`. `as usize` converts back safely (within usize range). `index_pack` receives a huge budget, all items fit, no truncation. `token_count` reports actual usage (small), `token_budget` reports `usize::MAX`. The budget is effectively disabled but no crash occurs. `count_tokens_batch` tokenizes all items regardless of budget — bounded by `limit.clamp(1, 10)` which limits items to ~50 total across sections.
- **Impact:** None — budget is effectively disabled. No OOM or crash.
- **Suggested mitigation:** Clamp `max_tokens` to reasonable max (e.g., 100_000) at entry. Low priority.

#### RT-RES-5: All graph traversals use visited sets — cycles cannot cause infinite loops
- **Severity:** none (verified safe)
- **Location:** `src/impact/bfs.rs:8-33`, `src/impact/bfs.rs:40-83`, `src/gather.rs:143-192`, `src/cli/batch/handlers.rs:606-623`, `src/cli/batch/handlers.rs:714-746`
- **Attack vector:** Call graph with mutual recursion: A calls B, B calls A.
- **PoC:** All five BFS implementations use HashMap-based visited sets. `reverse_bfs`: `!ancestors.contains_key(caller)` (line 24). `reverse_bfs_multi`: entry-based check with shortest-path update (lines 65-77). `bfs_expand`: `Entry::Vacant` / `Entry::Occupied` (lines 176-185). `dispatch_test_map`: `!ancestors.contains_key(caller)` (line 617). `dispatch_trace`: `!visited.contains_key(callee)` (line 740). All correctly terminate on cycles. **Verified safe across all five traversals.**
- **Impact:** None.
- **Suggested mitigation:** None needed.

#### RT-RES-6: Long query string safely truncated by tokenizer — no OOM risk
- **Severity:** none (verified safe)
- **Location:** `src/embedder.rs:249,496-501`
- **Attack vector:** `cqs task "$(python3 -c 'print("A" * 100000)')" --json`
- **PoC:** `Embedder.max_length = 512` (line 249). In `embed_batch` at line 496-501: `max_len = input_ids.iter().map(|v| v.len()).max().unwrap_or(0).min(self.max_length)`. Sequences longer than 512 tokens are truncated by `pad_2d_i64`. A 100KB query tokenizes to ~25K tokens, truncated to 512. ONNX tensor: `[1, 512, 768]` = 1.5MB — well within bounds. The tokenizer's `encode_batch` call at line 480-482 does produce the full token sequence in memory before truncation, but the tokenizers library handles this efficiently.
- **Impact:** None — truncation prevents OOM.
- **Suggested mitigation:** None needed.

#### RT-RES-7: Watch mode event queue bounded at both kernel and application level
- **Severity:** none (verified safe)
- **Location:** `src/cli/watch.rs:37,138`
- **Attack vector:** Rapid file creation: `for i in $(seq 1 100000); do touch src/test_$i.rs; done`
- **PoC:** `MAX_PENDING_FILES = 10_000` (line 37). At line 138: events beyond the cap are dropped. The `notify` crate uses OS inotify with kernel-level queue limit (`max_queued_events`, default 16384). `pending_files` is `HashSet` with `shrink_to(64)` after processing (line 159). The Rust `mpsc::channel()` is unbounded in theory, but event rate is limited by the kernel's inotify queue. **Verified safe.**
- **Impact:** None.
- **Suggested mitigation:** None needed.

#### RT-RES-8: `dispatch_test_map` chain reconstruction loop lacks iteration bound
- **Severity:** low
- **Location:** `src/cli/batch/handlers.rs:638-648`
- **Attack vector:** Hypothetical bug in BFS predecessor construction causing cyclic predecessor links
- **PoC:** The chain reconstruction at lines 638-648:
  ```rust
  while !current.is_empty() {
      chain.push(current.clone());
      if current == target_name { break; }
      current = ancestors.get(&current).map(|(_, p)| p.clone()).unwrap_or_default();
  }
  ```
  The `ancestors` HashMap is built by BFS (lines 606-623) using `!ancestors.contains_key(caller)`. BFS correctness guarantees acyclic predecessor links. **However**, the chain reconstruction loop has no safety bound — it relies entirely on BFS data being correct. If a future refactoring introduces a bug in the BFS (e.g., allowing predecessor updates), this loop could spin forever. The prior audit (RB-25) noted this was safe due to "max_depth + early exit" — but those are properties of the BFS, not of the chain loop itself.
- **Impact:** Infinite loop if predecessor data is cyclic. Requires a bug in the BFS to trigger. Extremely unlikely in current code.
- **Suggested mitigation:** Add `if chain.len() > max_depth + 2 { break; }` as a safety bound. One comparison per iteration, zero cost in the normal case.

#### RT-RES-9: `dispatch_task` reloads call graph and test chunks — bypasses BatchContext caches
- **Severity:** low
- **Location:** `src/cli/batch/handlers.rs:1455` calling `cqs::task()` at `src/task.rs:66-67`
- **Attack vector:** Pipeline: `search "common" | task` — 50 `task` dispatches, each reloading the full call graph
- **PoC:** `dispatch_task` calls the library `cqs::task()` function which internally calls `store.get_call_graph()` and `store.find_test_chunks()`. These are direct SQLite queries, not using `BatchContext`'s cached `call_graph` (OnceLock at mod.rs:57) or any test chunks cache. If a prior command loaded the graph via `ctx.call_graph()`, `dispatch_task` redundantly reloads it. In a pipeline with 50 fan-out, that's 50 graph loads + 50 test chunk loads. Each graph load is O(edges) — for 100K edges, ~10ms each = 500ms wasted. Already noted in CQ-3.
- **Impact:** Performance degradation in pipelines. Not a crash or OOM.
- **Suggested mitigation:** Add `test_chunks: OnceLock<Vec<ChunkSummary>>` to BatchContext. Create `task_with_resources()` accepting pre-loaded resources.

#### RT-RES-10: HNSW corrupted file — checksum verification prevents bincode deserialization panic
- **Severity:** low
- **Location:** `src/hnsw/persist.rs:28-78`, hnsw_rs `HnswIo::load_hnsw()`
- **Attack vector:** Manually corrupt `.hnsw.graph` or `.hnsw.data` file
- **PoC:** Blake3 checksums are verified before loading (lines 28-78). If checksums don't match, `Err(HnswError::ChecksumMismatch)` is returned before `hnsw_rs` deserialization. If no checksum file exists, `Err(HnswError::Internal("No checksum file..."))`. The only path to `load_hnsw()` goes through `verify_hnsw_checksums()` first. If checksums pass but data is somehow malformed (astronomically unlikely with blake3), `hnsw_rs` uses bincode which can panic on malformed input (RUSTSEC-2025-0141). **Checksum gate is the primary defense.**
- **Impact:** If checksums pass but data is corrupted: process panic. User re-runs `cqs index --force`. Not exploitable (trusted filesystem).
- **Suggested mitigation:** Consider `std::panic::catch_unwind()` around `load_hnsw()` for defense in depth. Low priority.

#### RT-RES-11: Empty query string properly rejected
- **Severity:** none (verified safe)
- **Location:** `src/embedder.rs:402-404`
- **Attack vector:** `cqs task "" --json`
- **PoC:** `embed_query` trims and checks empty at lines 402-404, returning `Err(EmbedderError::EmptyQuery)`. Propagates cleanly as `AnalysisError::Embedder`. No panic.
- **Impact:** None — clean error.
- **Suggested mitigation:** None needed.

#### RT-RES-12: `compute_modify_threshold` handles all-zero scores safely
- **Severity:** none (verified safe)
- **Location:** `src/scout.rs:308-341`
- **Attack vector:** Search results with all scores = 0.0
- **PoC:** Line 326: `if scores[i] > 0.0` guards the division, preventing `0.0 / 0.0 = NaN`. `best_gap` stays at 0.0. Returns `scores[0] = 0.0`. Edge case output quality already covered by AC-6.
- **Impact:** None.
- **Suggested mitigation:** Already covered by AC-6.

#### RT-RES-13: Waterfall surplus can cause `token_count > token_budget`
- **Severity:** medium
- **Location:** `src/cli/commands/task.rs:184`
- **Attack vector:** `cqs task "anything" --tokens 100 --json` with only notes content
- **PoC:** Already reported as AC-1. Notes section budget: `(budget * 0.10) + remaining` where `remaining` already includes the notes base allocation. If all previous sections empty: `remaining = 100`, `notes_budget = 10 + 100 = 110 > 100`.
- **Impact:** `token_count > token_budget`. Already covered by AC-1.
- **Suggested mitigation:** See AC-1 — serial deduction model.

### Silent Data Corruption (RT-DATA)

#### RT-DATA-1: Failed HNSW rebuild after watch reindex silently degrades search results
- **Severity:** medium
- **Location:** `src/cli/watch.rs:202-212`
- **Scenario:**
  1. `cqs watch` detects file changes, calls `reindex_files()` which updates chunks in SQLite (via `upsert_chunks_and_calls` — atomic per-file transaction).
  2. SQLite now has new chunk IDs for modified files (old IDs deleted, new IDs inserted).
  3. `build_hnsw_index()` is called at line 202. If it fails (e.g., disk full, permission error), execution falls through to the `Err` arm at line 210-212: `warn!(error = %e, "HNSW rebuild failed (search falls back to brute-force)")`.
  4. The HNSW `.bin` file on disk is now **stale** — it contains vectors for the old chunk IDs that no longer exist in SQLite, and lacks vectors for the new chunk IDs.
  5. Next search: `search_unified_with_index()` (search.rs:603-617) asks HNSW for candidates → HNSW returns old chunk IDs → `fetch_chunks_with_embeddings_by_ids_async()` does `SELECT ... WHERE id IN (...)` → old IDs return zero rows → results silently shrink.
  6. The user sees fewer results than expected with no error. The warning at line 211 says "search falls back to brute-force" but this is **incorrect** — the stale HNSW file remains on disk. The next `cqs search` (a separate process) loads the stale HNSW via `try_load()` and uses it. Only if the HNSW file were deleted would brute-force actually activate.
- **Corruption type:** Silent result set shrinkage. Valid matches are invisible until HNSW is successfully rebuilt.
- **Suggested mitigation:** On HNSW rebuild failure, delete the stale HNSW `.bin` file so that subsequent searches correctly fall back to brute-force (which reads directly from SQLite and returns complete results). Alternatively, store a generation counter in both SQLite and the HNSW metadata — reject the HNSW if generations mismatch.

#### RT-DATA-2: HNSW contains orphan vectors between SQLite prune and HNSW rebuild
- **Severity:** low
- **Location:** `src/cli/commands/gc.rs:35-51`
- **Scenario:**
  1. `cmd_gc` calls `store.prune_missing(&file_set)` at line 35, which deletes chunks from SQLite for files no longer on disk.
  2. At this point, the HNSW index still contains vectors for the pruned chunk IDs.
  3. If a search runs between line 35 (SQLite prune) and line 47-48 (HNSW rebuild), the HNSW returns orphan IDs that no longer exist in SQLite. `fetch_chunks_with_embeddings_by_ids_async` silently drops them.
  4. The window is small (gc holds `acquire_index_lock` at line 23, which prevents concurrent `index` and `watch` operations). But a concurrent `cqs search` from another terminal is NOT blocked by the index lock — search only reads, it doesn't acquire the write lock.
  5. Additionally: if `pruned_chunks == 0` but `pruned_calls > 0` or `pruned_type_edges > 0` (lines 39-44), the HNSW is NOT rebuilt at all (condition at line 47 only checks `pruned_chunks`). This is correct behavior (no chunks changed means HNSW is still valid), but call graph consumers (`callers`, `callees`, `impact`) will see stale data until the in-memory `CallGraph` is rebuilt.
- **Corruption type:** Transient silent result shrinkage during gc. Self-heals once HNSW rebuild completes.
- **Suggested mitigation:** Low priority — the gc window is brief and gc is user-initiated. Could delete HNSW file before SQLite prune to force brute-force during the window, but this adds latency to the common case.

#### RT-DATA-3: Watch reindex + concurrent search sees partially-updated SQLite
- **Severity:** low
- **Location:** `src/cli/watch.rs:190`, `src/store/chunks.rs:263-354`
- **Scenario:**
  1. `cqs watch` batches multiple changed files (watch.rs:157-175, debounce window).
  2. `reindex_files()` calls the pipeline which calls `upsert_chunks_and_calls()` per file — each file is a single SQLite transaction (chunks.rs:263-354).
  3. If 5 files changed, after file 3 is committed but before file 4 starts, a concurrent `cqs search` reads from SQLite (WAL mode allows concurrent readers).
  4. The search sees updated chunks for files 1-3 but stale chunks for files 4-5. HNSW hasn't been rebuilt yet (that happens at watch.rs:202 after all files).
  5. The search uses the old HNSW (returns old IDs for all 5 files) → hydrates from SQLite → files 1-3 return new chunks (old IDs gone, but HNSW still returns them → silently dropped), files 4-5 return old chunks (still in SQLite).
  6. Net effect: search results for the 5 modified files are missing or incomplete during the reindex window.
- **Corruption type:** Transient partial results during active reindexing. Self-heals after HNSW rebuild.
- **Suggested mitigation:** Acceptable for a development tool — the window is typically < 1 second for small batches. For large reindexes, the user is expected to wait for completion.

#### RT-DATA-4: Batch `call_graph()` cache silently serves stale data after index-mutating commands
- **Severity:** medium
- **Location:** `src/cli/batch/mod.rs:173-182`
- **Scenario:**
  1. Batch session starts: user sends `callers foo` → `ctx.call_graph()` loads the call graph from SQLite into `OnceLock`, returns fresh data.
  2. User sends a pipeline: `search "error" | callers` — for each search result, `dispatch_callers` calls `ctx.call_graph()` → returns cached (still fresh) data. Correct.
  3. User modifies the index outside the batch session: in another terminal, `cqs index` adds new files with new call edges.
  4. User returns to batch session, sends `callers bar` → `ctx.call_graph()` returns the **cached** call graph from step 1. New edges are invisible.
  5. The `dispatch_task` handler (handlers.rs:1425) calls `cqs::task()` which constructs its own `CallGraph` from scratch (bypassing the cache), so `task` commands see fresh data. But `callers`, `callees`, `test-map`, `impact`, `trace`, `related`, `dead` all use `ctx.call_graph()` and see stale data.
  6. The user sees inconsistent results: `task "add feature"` shows callers that `callers foo` doesn't, within the same session.
- **Corruption type:** Silent stale call graph data. User gets incomplete/inconsistent answers from different commands.
- **Prior finding:** DS-15 in v0.13.1 audit triage documented this as "intentional: batch is a session-scoped read-only view." However, the inconsistency with `task` (which bypasses the cache) was not documented. The mixed behavior — some commands cached, others fresh — is the actual data integrity concern.
- **Suggested mitigation:** Either (a) make `task` use the same cached call graph (consistent but stale), or (b) add a `refresh` batch command that resets all OnceLock caches, giving the user an explicit way to get fresh data. Option (a) is simpler and preserves the documented session-scoped semantics.

#### RT-DATA-5: HNSW search path does not check for NaN/Infinity scores — corrupts sort order
- **Severity:** medium
- **Location:** `src/hnsw/search.rs:49`
- **Scenario:**
  1. `hnsw-rs` `search_neighbours` returns a `Neighbour` with `distance` field.
  2. Line 49 computes `let score = 1.0 - n.distance`. If `n.distance` is NaN (which `hnsw-rs` can produce for degenerate vectors — zero-magnitude embeddings stored via `Embedding::new(vec![0.0; 769])`), then `score` is NaN.
  3. The `IndexResult { id, score }` with NaN score flows to `search_unified_with_index()` → `search_by_candidate_ids()`.
  4. `BoundedScoreHeap::push` (search.rs:329) checks `!score.is_finite()` and skips NaN scores. **This guards the brute-force path correctly.**
  5. However, `search_by_candidate_ids` does NOT use `BoundedScoreHeap` for HNSW-guided search. Instead it builds a `Vec<(ChunkRow, f32)>` (search.rs:669) and sorts with `partial_cmp...unwrap_or(Equal)` (search.rs:714). NaN scores survive and compare as Equal to everything, causing arbitrary sort order.
  6. The `rrf_fuse` path (store/mod.rs:674) also uses `partial_cmp...unwrap_or(Equal)` — NaN RRF scores would corrupt the fused ranking.
- **Corruption type:** Silent ranking corruption. Results exist but in wrong order, potentially pushing the best match below the truncation limit.
- **Trigger likelihood:** Low in practice — the ONNX model produces normalized embeddings (non-zero), and `build_batched` validates dimensions. However, `Embedding::new()` (unchecked constructor) is called from `store/chunks.rs:666` (loading embeddings from SQLite), `store/chunks.rs:966,1034,1341` (more loads), `store/notes.rs:339` (note embeddings), and `hnsw/mod.rs:298` (HNSW build). If the SQLite database contains a corrupt embedding row (e.g., all zeros from a failed embed), NaN propagates through all these paths.
- **Suggested mitigation:** Add `if !score.is_finite() { return None; }` guard at `hnsw/search.rs:49` (before constructing `IndexResult`), mirroring the `BoundedScoreHeap` guard. This single check prevents NaN from entering any downstream path.

#### RT-DATA-6: `partial_cmp...unwrap_or(Equal)` conflates NaN with equal scores in 4 sort sites
- **Severity:** low (dependent on RT-DATA-5)
- **Location:** `src/store/mod.rs:674` (rrf_fuse), `src/store/notes.rs:164` (note search), `src/diff.rs:181-186` (semantic diff), `src/search.rs:714` (candidate scoring)
- **Scenario:** All four sites sort `f32` scores using `partial_cmp(&sb).unwrap_or(Equal)`. If any score is NaN, `partial_cmp` returns `None`, and `unwrap_or(Equal)` treats NaN as equal to every other value. This violates sort transitivity (NaN == 0.5 AND NaN == 0.9, but 0.5 != 0.9), potentially causing undefined sort order depending on the sort algorithm's internal comparisons.
- **Corruption type:** Non-deterministic sort order when NaN scores are present. The same data can produce different rankings on different runs.
- **Note:** If RT-DATA-5's mitigation is applied (filter NaN at HNSW output), NaN never reaches these sort sites from the HNSW path. The brute-force path in `search_filtered` correctly uses `BoundedScoreHeap` (which filters NaN). These sites are primarily reachable via: (a) HNSW path without the RT-DATA-5 fix, (b) `rrf_fuse` if FTS returns NaN scores (unlikely — FTS uses BM25), or (c) `note_search` using brute-force cosine similarity on corrupt note embeddings.
- **Suggested mitigation:** Replace `unwrap_or(Equal)` with `f32::total_cmp()` (stable since Rust 1.62, cqs MSRV is 1.93) which defines a total order: NaN sorts after +Infinity. Single-line change at each site.

#### RT-DATA-7: `rewrite_notes_file` reads from separate open while holding exclusive lock on different fd
- **Severity:** low
- **Location:** `src/note.rs:185-222`
- **Scenario:**
  1. `rewrite_notes_file` opens `notes_path` read-only (line 185-188).
  2. Acquires exclusive lock on that fd (line 194).
  3. Reads content via `std::fs::read_to_string(notes_path)` (line 217) — this is a **separate** `open()` + `read()` syscall, not reading from the locked fd.
  4. Advisory locking on Linux (flock) is per-fd, not per-path. The exclusive lock on fd1 prevents other `rewrite_notes_file` callers from acquiring their own lock (they'll block at line 194). So the read at line 217 IS protected — no other writer can be active.
  5. However, the split between locked fd and read fd means: (a) two syscalls instead of one, (b) the code is misleading — a reader might assume the lock on fd1 somehow protects the separate read_to_string, which it does only indirectly.
- **Corruption type:** None found. The locking protocol is correct for the single-writer / multiple-reader pattern. The separate `read_to_string` is cosmetically odd but functionally safe because the exclusive lock prevents concurrent writes.
- **Suggested mitigation:** Minor cleanup — read from the locked fd instead of re-opening: `let mut content = String::new(); lock_file.read_to_string(&mut content)?;`. Eliminates the second open and is more obviously correct.

#### RT-DATA-8: `Embedding::new()` bypasses dimension validation — production load paths use unchecked constructor
- **Severity:** low
- **Location:** `src/embedder.rs:87-89`
- **Scenario:**
  1. `Embedding::new(data)` is unchecked — accepts any `Vec<f32>` regardless of length.
  2. `Embedding::try_new(data)` validates 769 dimensions — but is never called from production code (only from its own doctest).
  3. Production callsites using `Embedding::new()` include: `store/chunks.rs:666,966,1034,1341` (loading from SQLite), `store/notes.rs:339` (note embeddings), `hnsw/mod.rs:298` (HNSW build), `embedder.rs:573` (model output).
  4. If SQLite contains a corrupt embedding (truncated row, schema mismatch after partial migration), `Embedding::new()` wraps it without complaint.
  5. Downstream: `HnswIndex::search()` checks `query.len() != EMBEDDING_DIM` (hnsw/search.rs:29) and returns empty results on mismatch — but this checks the **query**, not stored vectors. `insert_batch` (hnsw/mod.rs:194-201) validates dimensions for HNSW insertion. The brute-force cosine similarity does NOT validate dimensions — `full_cosine_similarity(a, b)` with mismatched lengths iterates the shorter length, ignoring trailing dimensions of the longer vector.
  6. **Concrete scenario:** An old SQLite database with 768-dim embeddings (before sentiment dimension) is opened by current code. All `Embedding::new()` callsites wrap the 768-dim data. HNSW build rejects them (dimension check). But brute-force search (fallback when HNSW absent) computes cosine similarity between 769-dim query and 768-dim stored embedding, silently ignoring the 769th dimension (sentiment).
- **Corruption type:** Silent incorrect similarity scores. The sentiment dimension is ignored for mismatched embeddings, biasing results.
- **Suggested mitigation:** Add a dimension check in `full_cosine_similarity()`: if `a.len() != b.len()` return 0.0 with `tracing::warn!`. This is the single chokepoint where all similarity computations pass.

#### RT-DATA-9: Schema migration is atomic — no silent corruption on crash
- **Severity:** n/a (no finding)
- **Location:** `src/store/migrations.rs:43-56`
- **Analysis:**
  1. `run_migrations()` runs inside `IMMEDIATE` transaction (line 43-44). All DDL and DML execute within this transaction.
  2. The schema version is updated as the last step within the same transaction (line 53-55): `PRAGMA user_version = {target}`.
  3. If the process crashes mid-migration, SQLite rolls back the transaction. The `user_version` remains at the old value. On next startup, `check_schema` detects version mismatch and re-runs migration from scratch.
  4. SQLite WAL mode provides crash safety: uncheckpointed WAL frames are replayed on recovery.
  5. The only migration (v10 → v11) adds columns with defaults and creates new tables — idempotent DDL.
- **Corruption type:** None. The migration is correctly atomic.
- **Suggested mitigation:** None needed.
