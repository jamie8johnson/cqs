# Audit Findings — v1.13.0

## Documentation

#### DOC-1: Language count says 51 everywhere — should be 52 after IEC 61131-3 ST
- **Difficulty:** easy
- **Location:** README.md:5, README.md:502, README.md:579, Cargo.toml:6, CONTRIBUTING.md:86, src/lib.rs:16, src/nl/fields.rs:82
- **Description:** IEC 61131-3 Structured Text was added as the 52nd language (commit 4416593, PR #736, post-v1.13.0 release). All documentation still says "51 languages" — README TL;DR (line 5), README supported languages header (line 502), README "How It Works" (line 579), Cargo.toml description (line 6), CONTRIBUTING.md feature ideas (line 86), lib.rs doc comment (line 16), and nl/fields.rs doc comment (line 82). The README language list (`<details>` section lines 503-554) also omits the Structured Text entry.
- **Suggested fix:** Change "51" to "52" in all 7 locations. Add `- IEC 61131-3 Structured Text (.st, .stl files — function blocks, programs, functions, actions, methods)` to the README language list. Also add `structured_text.rs` to CONTRIBUTING.md architecture overview (line 130, after `aspx.rs, markdown.rs`).

#### DOC-2: language/mod.rs feature flag doc comments missing lang-st
- **Difficulty:** easy
- **Location:** src/language/mod.rs:62
- **Description:** The module-level doc comment lists all feature flags from `lang-rust` through `lang-aspx` but omits the `lang-st` feature flag added for IEC 61131-3 Structured Text. The list ends with `lang-aspx` then jumps to `lang-all`.
- **Suggested fix:** Add `//! - \`lang-st\` - IEC 61131-3 Structured Text support (enabled by default)` between the `lang-aspx` and `lang-all` lines.

#### DOC-3: lib.rs doc comment says "E5-base-v2 default, BGE-large preset" — reversed since v1.9.0
- **Difficulty:** easy
- **Location:** src/lib.rs:8
- **Description:** Line 8 says `configurable embedding models (E5-base-v2 default, BGE-large preset, custom ONNX)`. Since v1.9.0, BGE-large is the default and E5-base is the preset. This is the same class of bug as DOC-38/DOC-47 from the v1.12.0 triage, but in lib.rs which was not fixed.
- **Suggested fix:** Change to `(BGE-large-en-v1.5 default, E5-base preset, v9-200k LoRA preset, custom ONNX)`.

#### DOC-4: PRIVACY.md says E5-base-v2 is default, BGE-large is preset — reversed
- **Difficulty:** easy
- **Location:** PRIVACY.md:16, PRIVACY.md:26-27
- **Description:** Line 16 says "768 for E5-base-v2 default, 1024 for BGE-large" and line 26 says "Default: `intfloat/e5-base-v2` (~438MB)" with line 27 "Preset: `BAAI/bge-large-en-v1.5` (BGE-large)". Both are inverted since v1.9.0 when BGE-large became the default. Same class of bug as DOC-38.
- **Suggested fix:** Line 16: "1024 for BGE-large default, 768 for E5-base". Lines 26-27: swap Default/Preset labels. Add v9-200k as a third preset line.

#### DOC-5: SECURITY.md missing v9-200k model preset
- **Difficulty:** easy
- **Location:** SECURITY.md:41
- **Description:** The Network Requests section lists model download presets as "Default: BGE-large" and "Preset: e5-base" but omits the v9-200k LoRA preset added in v1.13.0. Since v9-200k downloads from a different HuggingFace repo (`jamie8johnson/e5-base-v2-code-search`), this is a disclosure gap — users checking SECURITY.md won't know about this network destination.
- **Suggested fix:** Add `- Preset: \`v9-200k\` (\`jamie8johnson/e5-base-v2-code-search\`, ~310MB)` after the e5-base line.

#### DOC-6: PRIVACY.md missing v9-200k model preset
- **Difficulty:** easy
- **Location:** PRIVACY.md:26-28
- **Description:** Same as DOC-5 but in PRIVACY.md. The Model Download section lists E5-base (incorrectly as default) and BGE-large (incorrectly as preset) but omits v9-200k entirely.
- **Suggested fix:** After fixing the default/preset labels per DOC-4, add a v9-200k preset line.

#### DOC-7: README eval section still references 55-query eval — replaced by 296-query eval
- **Difficulty:** medium
- **Location:** README.md:609
- **Description:** The "Retrieval Quality" section says "Evaluated on a hard eval suite of 55 queries across 5 languages (Rust, Python, TypeScript, JavaScript, Go)". CHANGELOG v1.13.0 says "Expanded pipeline eval — 296 queries across 7 languages (added Java + PHP). Replaces 55-query eval." The eval numbers (94.5% R@1 etc.) are from the old 55-query eval. The new expanded eval shows BGE-large at 90.9% R@1 and v9-200k at 90.5% R@1. The README should reflect the current eval.
- **Suggested fix:** Update the eval description to reference 296 queries across 7 languages. Update the table numbers to match the expanded eval results. Add v9-200k column. Note: the headline "94.5% R@1" in the TL;DR and Cargo.toml description may need updating too if it no longer reflects the current eval — or note it's from the legacy eval.

#### DOC-8: store/migrations.rs v14→v15 migration doc comment says "768-dim E5-base-v2"
- **Difficulty:** easy
- **Location:** src/store/migrations.rs:172-174
- **Description:** The doc comment for the v14→v15 migration function says "768-dim embeddings (SQ-9)" and "embeddings are now pure 768-dim E5-base-v2 output". This was correct at the time of writing but is misleading now — the current default is BGE-large at 1024-dim. Same class as DOC-44 from v1.12.0 triage.
- **Suggested fix:** This is historical documentation for a migration. Add a note: "Note: this migration predates the v1.9.0 switch to BGE-large (1024-dim) as default. The migration itself is correct for indexes created with E5-base-v2."

#### DOC-9: CHANGELOG missing IEC 61131-3 Structured Text entry
- **Difficulty:** easy
- **Location:** CHANGELOG.md:8
- **Description:** The `[Unreleased]` section of CHANGELOG.md is empty, but PR #736 (commit 4416593) added IEC 61131-3 Structured Text as the 52nd language after the v1.13.0 release. This should appear in the Unreleased section.
- **Suggested fix:** Add under `[Unreleased]`: `### Added\n- **IEC 61131-3 Structured Text** (.st, .stl) — 52nd language. Function blocks, programs, functions, actions, methods with qualified naming.`

#### DOC-10: Cargo.toml description says "51 languages" and cites 55-query eval metrics
- **Difficulty:** easy
- **Location:** Cargo.toml:6
- **Description:** The crate description on crates.io says "51 languages, 94.5% Recall@1 (BGE-large), 0.966 MRR". Language count is now 52. The eval metrics are from the legacy 55-query suite — the expanded 296-query eval shows different numbers.
- **Suggested fix:** Update to "52 languages". Either update metrics to match the expanded eval or note "(55-query eval)" to avoid confusion.

## API Design

#### AD-1: `cmd_blame` parameter ordering inconsistent — `json` before domain params
- **Difficulty:** easy
- **Location:** src/cli/commands/blame.rs:246
- **Description:** `cmd_blame(target, json, depth, show_callers)` puts `json` as the second parameter, between the target name and domain-specific parameters. Every other cmd_ function places `json` as the last positional parameter (e.g., `cmd_similar(cli, target, limit, threshold, json)`, `cmd_deps(name, reverse, json)`, `cmd_callers(name, json)`, `cmd_test_map(name, depth, json)`). The dispatch call at `src/cli/dispatch.rs:50` has to reorder: `cmd_blame(name, json, depth, callers)` when the destructured fields come in the order `name, depth, callers, json`.
- **Suggested fix:** Change signature to `cmd_blame(target, depth, show_callers, json)` and update the dispatch call. One-line change at each site.

#### AD-2: `name` vs `target` inconsistent for function-identifier CLI parameters
- **Difficulty:** medium
- **Location:** src/cli/definitions.rs (multiple variants)
- **Description:** Commands that take a function identifier use `name` in some variants and `target` in others. `Blame`, `Explain`, `Callers`, `Callees`, `Deps`, `Neighbors`, `TestMap`, `Related` all use `name: String`. `Similar` uses `target: String`. `Trace` uses `source` and `target`. The dispatch layer also varies: `cmd_blame` calls it `target`, `cmd_similar` calls it `target`, but the struct field is `name` for blame and `target` for similar. `Similar`'s `target` is defensible (it's a reference point, not a search query), but the inconsistency between `name` in the enum and `target` in the handler is gratuitous.
- **Suggested fix:** Standardize on `name` for single-function commands (Blame, Explain, Similar, etc.) and reserve `source`/`target` for two-function commands (Trace). This is a breaking change for the `Similar` variant but the batch command already uses `target` so both would need updating. Low priority given no external users.

#### AD-3: `root` vs `project_root` parameter naming split across analysis functions
- **Difficulty:** easy
- **Location:** src/gather.rs:393, src/scout.rs:133, src/task.rs:74, src/onboard.rs:97, src/impact/analysis.rs:35, src/review.rs:81, src/suggest.rs:56
- **Description:** Library analysis functions use two different names for the same parameter: `root` (scout, onboard, task, review) vs `project_root` (gather, suggest). Both are `&Path` pointing to the project root directory. `gather` is especially inconsistent — the public function uses `project_root` but the internal `gather_with_graph` also uses `project_root`, while every other analysis function uses `root`.
- **Suggested fix:** Standardize on `root` (shorter, already more common). Rename `project_root` to `root` in `gather.rs` (5 occurrences) and `suggest.rs` (4 occurrences). Internal-only change, no CLI impact.

#### AD-4: `gather()` takes pre-computed `query_embedding + query_text` while peer functions take `embedder + description`
- **Difficulty:** medium
- **Location:** src/gather.rs:388-394
- **Description:** All analysis functions follow the pattern `(store, embedder, text, root, limit)` — scout, task, onboard, suggest_placement, plan all take an `&Embedder` and embed the query internally. `gather()` breaks this pattern by requiring the caller to pre-compute the embedding: `(store, query_embedding, query_text, opts, project_root)`. This forces every caller (cmd_gather, task_with_resources, batch handler) to manually call `embedder.embed_query()` and pass both the text and the embedding. The design exists because `task()` shares a single embedding across scout+gather phases, but the inconsistency complicates the API surface.
- **Suggested fix:** Add a convenience `gather_simple(store, embedder, query_text, opts, root)` that embeds internally, matching the other functions. Keep `gather()` for callers that pre-compute embeddings (task, batch). Or rename current `gather` to `gather_with_embedding` and make `gather` the convenience version.

#### AD-5: 26 commands use bare `json: bool` field instead of `OutputArgs`/`TextJsonArgs`
- **Difficulty:** medium
- **Location:** src/cli/definitions.rs (26 command variants)
- **Description:** The v1.12.0 triage identified AD-49 (`--json` vs `--format` inconsistency) and `OutputArgs`/`TextJsonArgs` were added to address it. But only 4 commands use the shared structs: Impact and Trace use `OutputArgs`, Review and Ci use `TextJsonArgs`. The remaining 26 commands (Brief, Stats, Affected, Blame, Deps, Callers, Callees, Onboard, Neighbors, Diff, Drift, Explain, Similar, ImpactDiff, TestMap, Context, Dead, Gather, Gc, Health, AuditMode, Stale, Suggest, Read, Related, Where, Scout, Plan, Task) still have a bare `#[arg(long)] json: bool` field. This means those commands cannot accept `--format text` or `--format json` — only `--json`. Users who learn the `--format` pattern from impact/trace/review/ci will get "unexpected argument" on all other commands.
- **Suggested fix:** Migrate the 26 bare-`json` commands to use `TextJsonArgs` (they don't support mermaid). This is mechanical: replace `json: bool` with `#[command(flatten)] output: TextJsonArgs`, update dispatch to call `output.effective_format()`, and update handlers to accept `&OutputFormat` instead of `bool`. Low risk, high consistency gain. Can be done incrementally.

#### AD-6: `ScoutOptions` and `PlacementOptions` have different default construction patterns
- **Difficulty:** easy
- **Location:** src/scout.rs:101-123, src/where_to_add.rs:67-90
- **Description:** `ScoutOptions` has a manual `Default` impl with inline defaults and builder methods. `PlacementOptions` has a `#[derive(Default)]` and builder methods. `GatherOptions` has a manual `Default` impl. All three are configuration structs for analysis functions but use different patterns. `ScoutOptions::default()` sets `search_limit: 20, search_threshold: 0.15, seed_limit: 10`, while constants `DEFAULT_SCOUT_SEARCH_LIMIT` (20) and `DEFAULT_SCOUT_SEARCH_THRESHOLD` (0.15) exist but aren't used in the Default impl (they're used in the constants only). This is fragile — changing the constant doesn't change the default.
- **Suggested fix:** Have `ScoutOptions::default()` use the constants: `search_limit: DEFAULT_SCOUT_SEARCH_LIMIT`. Same for `GatherOptions` which has `DEFAULT_MAX_EXPANDED_NODES` but hardcodes `200` in Default. Trivial fix, prevents drift between constants and defaults.

#### AD-7: `embedding_slice` and `bytes_to_embedding` return `Option` where `Result` would be more informative
- **Difficulty:** medium
- **Location:** src/store/helpers.rs:882, src/store/helpers.rs:900
- **Description:** Both functions return `Option<&[f32]>` / `Option<Vec<f32>>` when they fail on dimension mismatch. The only failure mode is a byte-length mismatch, which is a data integrity issue (corrupted or truncated embedding). Every caller uses `let Some(emb) = ... else { continue }` or `.filter_map()` to silently skip bad embeddings. The `trace!` log inside these functions is the only evidence of corruption. Meanwhile, `embedding_to_bytes` (the write path) returns `Result<Vec<u8>, StoreError>` for the same kind of validation. The asymmetry means read-path corruption is silent while write-path corruption is an error.
- **Suggested fix:** This is noted as a known design choice — the functions are on hot search paths where `Result` would add overhead (error formatting) and the callers genuinely want to skip bad entries. The `Option` is appropriate here. However, a `warn!`-level log on first occurrence per search (instead of `trace!` on every occurrence) would make corruption discoverable without flooding logs. **Retract as non-finding** — the current design is intentional.

#### AD-8: `analyze_impact` takes individual parameters while peer functions use options structs
- **Difficulty:** medium
- **Location:** src/impact/analysis.rs:30-36
- **Description:** `analyze_impact(store, target_name, depth, include_types, root)` takes 5 positional parameters including two booleans and a usize. Peer functions use options structs: `scout_with_options(store, embedder, task, root, limit, &ScoutOptions)`, `suggest_placement_with_options(store, embedder, desc, limit, &PlacementOptions)`, `gather(store, emb, text, &GatherOptions, root)`. The CLI `ImpactArgs` struct already exists with these fields, but it lives in `src/cli/args.rs` (CLI layer), not in the library. The library function unpacks the struct at the CLI boundary, losing the grouping.
- **Suggested fix:** Create an `ImpactOptions` struct in `src/impact/analysis.rs` with `depth`, `include_types`, and `suggest_tests` fields. Change `analyze_impact` to take `&ImpactOptions`. The CLI `ImpactArgs` can derive from or convert to `ImpactOptions`. This matches the pattern established by `ScoutOptions`, `GatherOptions`, and `PlacementOptions`.

#### AD-9: `Embedding::new` accepts any data without validation, `try_new` name suggests fallibility is exceptional
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:101-103, src/embedder/mod.rs:123
- **Description:** Already flagged as AD-54 in v1.12.0 triage. Current state: `new()` is documented as "unchecked, use for known-good data" and `try_new()` is "use for untrusted input". The doc comments are now clear (lines 96-100), but the naming still invites misuse — Rust convention is `new` for the standard constructor, not the unsafe-ish one. 22 callers of `Embedding::new()` vs 3 of `try_new()` — the unchecked path is the common path by design (all internal, from ONNX inference).
- **Suggested fix:** Skip — already triaged as AD-54. The current docs adequately warn callers. Renaming would be churn.

#### AD-10: `search_single_project` returns `Option<Vec<...>>` hiding search failures
- **Difficulty:** easy
- **Location:** src/project.rs:276
- **Description:** `search_single_project` returns `Option<Vec<CrossProjectResult>>` where `None` means "project couldn't be searched" (index missing, store open failed, etc.). The caller uses `.filter_map()` to silently drop failed projects. Individual failures are logged at `warn!` level, but the aggregated result has no way to surface "3 of 5 projects failed" to the user. The function body has multiple early `return None` paths: index not found (line 286), store open failure (line 300), HNSW load failure (silently continues), search failure (line 320).
- **Suggested fix:** Return `Result<Vec<CrossProjectResult>, ProjectError>` and let the caller decide whether to collect errors or filter. The caller can still `.filter_map(|r| r.ok())` but now has the option to report failures. Low priority — cross-project search is a secondary feature.

## Error Handling

#### EH-1: `serde_json::to_value().ok()` silently drops serialization errors in 7 locations
- **Difficulty:** easy
- **Location:** src/task.rs:291,296,301; src/cli/commands/task.rs:469,479,489; src/impact/format.rs:28
- **Description:** Seven `serde_json::to_value(x).ok()` calls silently drop serialization errors via `filter_map`. In the same file (`task.rs:280-286`), the code section uses the correct pattern: `match serde_json::to_value(c) { Ok(v) => Some(v), Err(e) => { tracing::warn!(...); None } }`. The risk/tests/placement sections skip this logging. If a struct fails to serialize (e.g., a NaN in a float field), the entry silently vanishes from output. The `impact/format.rs:28` instance is the same pattern for test serialization in impact JSON output.
- **Suggested fix:** Replace `.ok()` with the same `match` + `tracing::warn!` pattern used for code chunks. Mechanical change — copy the pattern from lines 280-286 and adjust field names.

#### EH-2: `try_acquire_index_lock()` treats all `try_lock()` errors as "lock held"
- **Difficulty:** easy
- **Location:** src/cli/files.rs:110
- **Description:** `try_lock()` returns `Err` for both `WouldBlock` (lock held by another process — expected) and real I/O errors (permission denied, filesystem failure — unexpected). The current code `Err(_) => Ok(None)` maps both to "couldn't acquire lock, skip." This means a permission-denied error on the lock file silently causes the caller to skip work, with no log or user indication. The caller (`process_file_changes` in watch.rs) interprets `None` as "another indexer is running" and skips the entire reindex cycle.
- **Suggested fix:** Match on the error kind: `Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None)` for the expected case, `Err(e) => { tracing::warn!(error = %e, "Unexpected lock error"); Ok(None) }` for others. Or use `Err(e) => Err(e.into())` to propagate real failures.

#### EH-3: `ci.rs` `find_dead_code` failure degrades to empty vec without user notification
- **Difficulty:** easy
- **Location:** src/ci.rs:124-127
- **Description:** When `store.find_dead_code(true)` fails in `run_ci_analysis`, the error is caught with `Err(e) => { tracing::warn!(...); Vec::new() }`. The CI report then shows zero dead code — which looks like a clean result. The JSON output has no field to distinguish "no dead code found" from "dead code scan failed." A CI consumer checking `dead_in_diff.len() == 0` would get a false pass.
- **Suggested fix:** Add a `dead_code_error: Option<String>` field to `CiReport` and populate it on failure. Alternatively, include it in the existing `warnings` field of `ReviewResult`. The user should see "dead code scan failed: {error}" in CI output.

#### EH-4: `build_brief_data` degrades silently on call-graph/test-chunk load failure
- **Difficulty:** easy
- **Location:** src/cli/commands/brief.rs:51-67
- **Description:** Three store operations use `unwrap_or_else(|e| { warn!(...); default })`: `get_caller_counts_batch` (line 51), `get_call_graph` (line 57), and `find_test_chunks` (line 64). All three degrade to showing zero callers/tests. The function returns `Result<BriefData>` — it could propagate these errors. The warn-and-degrade pattern is intentional for `suggest_notes` (where partial results are useful), but `brief` is showing a function-level summary where zero callers/tests is misleading if the actual data is just inaccessible.
- **Suggested fix:** Use `?` to propagate. The function already returns `Result`. If degraded output is preferred, add a `warnings: Vec<String>` field to `BriefData` and surface them in the output.

#### EH-5: `set_permissions` failures silently ignored in 6 locations
- **Difficulty:** easy
- **Location:** src/project.rs:124; src/config.rs:406,487; src/audit.rs:126,156; src/cli/commands/export_model.rs:154
- **Description:** Six `let _ = std::fs::set_permissions(...)` calls silently ignore failure. These set restrictive permissions (0o600 or 0o700) on config files, audit state, project registry, and model exports (SEC-19 hardening). On non-Unix platforms the call is gated by `#[cfg(unix)]` which is correct. However, on Unix, a permission failure (e.g., on a read-only filesystem, or a file owned by another user) means the security hardening silently fails. The config and audit files may contain sensitive data (API keys in config, model paths in audit state).
- **Suggested fix:** Log at `debug!` level on failure: `if let Err(e) = std::fs::set_permissions(...) { tracing::debug!(error = %e, "Failed to set permissions"); }`. Don't propagate as an error — the file was already written successfully, and failing to restrict permissions shouldn't prevent the operation. But silent ignore means nobody knows the hardening didn't apply.

#### EH-6: `index_notes_from_file` converts parse error to `Ok((0, false))` — caller can't distinguish "no notes" from "parse failed"
- **Difficulty:** easy
- **Location:** src/cli/commands/index.rs:390-393
- **Description:** When `parse_notes(&notes_path)` fails (malformed TOML), the error is logged at `warn!` and the function returns `Ok((0, false))`. The caller (`cmd_index`) sees "0 notes indexed, not skipped" which is identical to "empty notes file." The `--quiet` flag suppresses the "0 notes" message entirely. A user with a typo in `notes.toml` gets no indication that their notes weren't indexed — they just don't appear in search results.
- **Suggested fix:** Return `Ok((0, false))` is fine for the non-blocking behavior, but the caller should print a user-visible warning even in non-quiet mode: "Warning: notes.toml parse error — notes not indexed." Currently only `tracing::warn!` fires, which requires `RUST_LOG=warn` to see.

## Code Quality

#### CQ-1: `dispatch_trace` re-implements BFS shortest path instead of calling `bfs_shortest_path`
- **Difficulty:** easy
- **Location:** src/cli/batch/handlers/graph.rs:338-371
- **Description:** `dispatch_trace` contains an inline BFS shortest-path implementation (34 lines) that is functionally identical to `bfs_shortest_path` in `src/cli/commands/trace.rs:198-237`. Both implement the same algorithm: visited map with predecessor tracking, VecDeque-based BFS, path reconstruction by walking predecessors. The only difference is `dispatch_trace` inlines the BFS while `cmd_trace` calls the extracted function. This is copy-paste duplication that will drift over time — if a bug is found in one, the other won't get fixed.
- **Suggested fix:** Move `bfs_shortest_path` to a shared location (e.g., `src/impact/bfs.rs` which already has BFS utilities, or `src/store/calls/query.rs`). Have both `cmd_trace` and `dispatch_trace` call the shared function. ~34 lines removed from graph.rs, ~0 lines added.

#### CQ-2: `dispatch_test_map` duplicates entire reverse-BFS + test-matching algorithm from `cmd_test_map`
- **Difficulty:** medium
- **Location:** src/cli/batch/handlers/graph.rs:197-290, src/cli/commands/test_map.rs:8-89
- **Description:** Both functions implement the same 80-line algorithm: (1) resolve target, (2) reverse BFS to build ancestors map, (3) match test chunks against ancestors, (4) reconstruct call chains, (5) sort by depth, (6) format as JSON. They even both define an identical private `struct TestMatch { name, file, line, depth, chain }` (graph.rs:230, test_map.rs:43). The chain-walking code has a subtle divergence: `dispatch_test_map` has a `chain_limit` guard (`max_depth + 1`) that `cmd_test_map` lacks — if this is a bug fix, it wasn't applied to both copies.
- **Suggested fix:** Extract a shared `find_test_map(store, graph, test_chunks, target_name, max_depth) -> Vec<TestMatch>` function in the library layer (e.g., `src/impact/analysis.rs` alongside `find_affected_tests_with_chunks`). Both cmd and dispatch call it, then format the result. The `TestMatch` struct becomes a shared type. Eliminates ~80 lines of duplication and the chain-limit divergence.

#### CQ-3: `cmd_impact` and `dispatch_impact` duplicate test-suggestion JSON construction
- **Difficulty:** easy
- **Location:** src/cli/commands/impact.rs:42-60, src/cli/batch/handlers/graph.rs:157-176
- **Description:** Both functions contain identical code to convert `Vec<TestSuggestion>` to a JSON array and insert it into an impact JSON object. The 18-line block maps each suggestion to `json!({"test_name", "suggested_file", "for_function", "pattern_source", "inline"})` and inserts under `"test_suggestions"`. This is a specific instance of the systemic cmd/batch duplication.
- **Suggested fix:** Add a `fn test_suggestions_to_json(suggestions: &[TestSuggestion]) -> Vec<serde_json::Value>` helper in `src/impact/mod.rs` (alongside existing `impact_to_json` and `impact_to_mermaid`). Both cmd and dispatch call it. ~15 lines removed from each site.

#### CQ-4: `cmd_test_map` computes unused `_test_names` HashSet
- **Difficulty:** easy
- **Location:** src/cli/commands/test_map.rs:20
- **Description:** Line 20 computes `let _test_names: HashSet<String> = test_chunks.iter().map(|t| t.name.clone()).collect();` — this clones every test chunk name into a HashSet that is never used. The underscore prefix suppresses the dead-code warning. This allocates and immediately drops O(N) strings on every `cqs test-map` invocation.
- **Suggested fix:** Delete line 20. One-line fix.

#### CQ-5: `cmd_trace` uses N individual `search_by_name` calls where `dispatch_trace` uses batched query
- **Difficulty:** easy
- **Location:** src/cli/commands/trace.rs:68-82
- **Description:** `cmd_trace` resolves trace path nodes by calling `store.search_by_name(name, 1)` in a loop (one SQL query per node). The batch handler `dispatch_trace` (graph.rs:376-377) was upgraded to use `store.search_by_names_batch(&name_refs, 1)` (single batched query). The CLI path was not upgraded. For a trace with 10 hops, this is 10 round-trips vs 1.
- **Suggested fix:** Replace the loop in `cmd_trace` with `search_by_names_batch`, matching the batch handler pattern. The batch function already exists. ~5 lines changed.

#### CQ-6: `filter_by_pattern` is dead code with only a test caller
- **Difficulty:** easy
- **Location:** src/structural.rs:249-260
- **Description:** `filter_by_pattern` is a public function annotated with `#[allow(dead_code)]` and the comment "Public API -- used in tests, available for external consumers". `cqs callers` confirms zero production callers — only `test_filter_by_pattern` (structural.rs:447) calls it. The project has no external consumers (MEMORY.md: "nobody else is using cqs but us"). This is code kept "in case" but with no integration point.
- **Suggested fix:** Delete the function and its test. If the pattern-filtering concept is needed later, it can be re-derived from `Pattern::matches` which is the actual used API. ~20 lines removed.

#### CQ-7: Batch handlers total 2128 lines mirroring CLI commands — no shared JSON serialization layer
- **Difficulty:** hard
- **Location:** src/cli/batch/handlers/ (6 files, 2128 lines)
- **Description:** The batch handler layer (`dispatch_*` functions) and the CLI command layer (`cmd_*` functions) both contain JSON construction logic for the same data. Some handlers correctly delegate to shared functions (e.g., `dispatch_explain` calls `build_explain_data` + `explain_to_json`, `dispatch_context` calls `build_compact_data` + `compact_to_json`). Others re-implement the logic (dispatch_trace, dispatch_test_map, dispatch_impact's suggestion block). The pattern that works should be the standard. CQ-1 through CQ-3 are specific instances of this systemic issue.
- **Suggested fix:** For each command pair where the batch handler re-implements logic: (1) extract a `build_X_data` function that returns a typed struct, (2) extract an `X_to_json` function that serializes it, (3) have both `cmd_X` and `dispatch_X` call the shared functions. The full sweep would cover trace, test_map, callers, callees, deps, stale, dead, diff, drift, similar, and search. Estimate: ~400 lines of duplication can be eliminated.

#### CQ-8: Four `clippy::too_many_arguments` suppressions remain (down from 9)
- **Difficulty:** medium
- **Location:** src/cli/commands/gather.rs:10, src/cli/commands/query.rs:133, src/scout.rs:174, src/cli/batch/handlers/misc.rs:30
- **Description:** Four functions suppress `clippy::too_many_arguments`. The v1.12.0 triage noted CQ-39 ("Nine clippy::too_many_arguments suppressions") — 5 have been fixed, but 4 remain. `cmd_gather` takes 8 params where the last 6 are gather-specific options. `cmd_query_project` takes 9 params mixing infrastructure (store, embedder, cqs_dir) with query params. `scout_core` takes 8 params with pre-loaded resources. `dispatch_gather` takes 7 params.
- **Suggested fix:** For `cmd_gather`: thread `GatherOptions` through instead of unpacking at the CLI boundary. For `cmd_query_project`: create a `QueryContext { store, cqs_dir, root, embedder }` struct. For `scout_core`: create a `ScoutContext { store, graph, test_chunks }` for pre-loaded resources. Each is a ~20-line refactor.

#### CQ-9: lib.rs re-exports ~70 items from `pub(crate)` modules as "not public API"
- **Difficulty:** medium
- **Location:** src/lib.rs:121-162
- **Description:** 25 `pub use` statements re-export approximately 70 types and functions from `pub(crate)` modules. The comment says "Re-exports for binary crate (CLI) - these are NOT part of the public library API". This grows with each new analysis command — the impact module alone exports 25 items (lines 129-136). Every new feature adds 3-5 more re-exports, creating dual maintenance (module + lib.rs).
- **Suggested fix:** Since there are no external consumers, consider making the internal modules `pub` with `#[doc(hidden)]` instead of `pub(crate)` + explicit re-exports. Or use `pub use module::*` for internal modules to avoid itemizing. Lower priority — this is maintenance friction, not a bug.

## Observability

#### OB-1: `plan()` missing tracing span — invisible in flame graphs
- **Difficulty:** easy
- **Location:** src/plan.rs:378
- **Description:** `plan()` is a public library function that calls `classify()` + `scout()` but has no `tracing::info_span!` at entry. Every peer analysis function has one: `scout()` (scout.rs:143), `task()` (task.rs:76), `onboard()` (onboard.rs:100), `gather()` (gather.rs:418), `review_diff()` (review.rs:83), `suggest_notes()` (suggest.rs:57). The `plan` function is called from `cmd_plan` which also lacks a span (the CLI handler relies on the library span). This means `cqs plan` execution is invisible in tracing output and flame graphs — you see `scout` inside it but not the `plan` wrapper.
- **Suggested fix:** Add `let _span = tracing::info_span!("plan", description_len = description.len()).entered();` at the top of `plan()`. One-line addition.

#### OB-2: `create_client` and `LlmClient::new` missing tracing spans — LLM initialization invisible
- **Difficulty:** easy
- **Location:** src/llm/mod.rs:291, src/llm/mod.rs:329
- **Description:** `create_client()` is the LLM client factory that reads `ANTHROPIC_API_KEY` from the environment and constructs an HTTP client. Neither it nor `LlmClient::new()` has a tracing span. When LLM features fail (API key missing, HTTP client build failure), the error propagates but there's no span to locate the failure in tracing output. The peer `LlmConfig::resolve()` at line 184 does have a span (`resolve_llm_config`). Since `create_client` + `new` involve environment access and HTTP client construction (two external failure points), they should be instrumented.
- **Suggested fix:** Add `let _span = tracing::info_span!("create_llm_client", provider = ?llm_config.provider).entered();` in `create_client()`. `LlmClient::new` is trivial (just `Client::builder().build()`) and can share the parent span.

#### OB-3: `delete_phantom_chunks` missing tracing span — silent chunk deletion in watch path
- **Difficulty:** easy
- **Location:** src/store/chunks/crud.rs:487
- **Description:** `delete_phantom_chunks` is called during watch reindex to clean up stale chunks when a file's chunk set changes. It creates a temp table, batch-inserts live IDs, and deletes non-matching chunks — a multi-step SQL operation. Every other mutation function in crud.rs has a span: `upsert_chunk` (line 69), `upsert_chunks_batch` (line 43), `delete_by_origin` (line 397), `upsert_chunks_and_calls` (line 431). The gap means phantom chunk deletion is invisible in tracing, even though it's a data-modifying operation that could silently remove chunks if `live_ids` is wrong.
- **Suggested fix:** Add `let _span = tracing::info_span!("delete_phantom_chunks", file = %file.display(), live_count = live_ids.len()).entered();` at line 492 (after `origin_str` assignment). Include the file path and live_ids count for debugging.

#### OB-4: `search_filtered` outer wrapper has no span — only the inner `search_filtered_with_notes` is instrumented
- **Difficulty:** easy
- **Location:** src/search/query.rs:59
- **Description:** `search_filtered` is the primary public search API (the one CLAUDE.md says all user-facing search should go through). It loads cached notes, then delegates to `search_filtered_with_notes` which has the `info_span!("search_filtered")`. The problem: when note loading fails (line 70), the `warn!` fires but there's no enclosing span to correlate it with. A tracing subscriber sees a bare `warn!` without parent context. Adding a span to the outer function would group the note-load warning with the search it belongs to.
- **Suggested fix:** Add `let _span = tracing::info_span!("search_filtered", limit, threshold = %threshold).entered();` at the top of the outer `search_filtered()`. Rename the inner span to `search_filtered_inner` or `search_filtered_core` to avoid duplicate span names.

#### OB-5: Batch mode `get_ref` loads reference index without elapsed timing
- **Difficulty:** easy
- **Location:** src/cli/batch/mod.rs:235-269
- **Description:** `BatchContext::get_ref()` opens a store, loads an HNSW index (potentially 800MB), and stores it in the context. It has no timing instrumentation — the only log is `info!("reference loaded")` at the end (line 268). For a large reference index, this could take 5-10 seconds with no progress indication. The peer operations `embedder()` (line 199), `vector_index()` (line 222), and `call_graph()` (line 344) all have `info_span!` with timing-relevant field names. `get_ref` is the only lazy-init method without one.
- **Suggested fix:** Add `let _span = tracing::info_span!("batch_ref_load", name).entered();` at the top of `get_ref()`.

#### OB-6: v1.12.0 carryover OB-28 through OB-32 are all resolved
- **Difficulty:** n/a
- **Location:** various
- **Description:** Verification of prior findings: OB-28 (`detect_provider`/`create_session`) now have spans at src/embedder/provider.rs:220,252. OB-29 (`parse_unified_diff`) has span at src/diff_parse.rs:35. OB-30 (`find_changed_functions`) has span at src/train_data/diff.rs:132. OB-31 (`load_audit_state`/`save_audit_state`) have spans at src/audit.rs:71,107. OB-32 (`update_embeddings_batch` silent on zero-row) now logs `debug!` at src/store/chunks/crud.rs:89. All 5 carryover findings are fixed. Mark as resolved in triage.
- **Suggested fix:** Update triage table to mark OB-28 through OB-32 as resolved.

## Algorithm Correctness

#### AC-1: Cross-project score comparison invalid — RRF scores are rank-relative, not absolute
- **Difficulty:** medium
- **Location:** src/project.rs:253-257
- **Description:** `search_across_projects` collects `SearchResult` from each project via `search_filtered_with_index` (which uses RRF fusion internally), then sorts all results globally by `score` and truncates to `limit`. However, RRF scores are rank-based (`1/(K+rank)`) and only meaningful within a single ranking — a score of 0.032 from a 5000-chunk project is not comparable to 0.032 from a 50-chunk project. The semantic embedding scores that feed into RRF are also project-local (different embedding models or dimensions across projects make raw cosine scores incomparable). The global sort produces an unpredictable mix that favors projects with fewer chunks (where top-ranked items get the full RRF contribution without dilution from competitors).
- **Suggested fix:** Either (1) normalize scores per-project before merging (e.g., divide by max score within each project's results), or (2) use a round-robin interleaving strategy (take rank-1 from each project, then rank-2, etc.), or (3) re-score all cross-project candidates against the query embedding using `full_cosine_similarity` for a uniform comparison basis. Option (3) is most correct but requires loading embeddings across project boundaries.

#### AC-2: `score_candidate` negative scores invert note boost and demotion semantics
- **Difficulty:** medium
- **Location:** src/search/scoring/candidate.rs:231-261
- **Description:** The name_boost interpolation `(1-nb)*embedding_score + nb*name_score` can produce negative base scores when `embedding_score < 0` (cosine similarity of L2-normalized vectors ranges [-1, 1]). With `name_boost=0.3`, `embedding_score=-0.5`, `name_score=0.0`: base = `0.7*(-0.5) + 0.3*0 = -0.35`. This negative score then gets multiplied by the note boost and importance demotion. A positive note (boost=1.15) makes the negative score *more negative* (-0.35 * 1.15 = -0.4025), and test demotion (0.7x) actually *improves* the score (-0.35 * 0.7 = -0.245). Both effects are inverted from their intent. The negative score can also pass a threshold of 0.0 and enter the BoundedScoreHeap.
- **Suggested fix:** Clamp `base_score` to `max(0.0, base_score)` before applying multiplicative adjustments. Negative cosine similarity means the vectors are opposed — the chunk is maximally irrelevant and should be filtered out, not scored. Alternatively, apply note boost and importance as additive rather than multiplicative adjustments when the base score is negative, though clamping to zero is simpler and more correct.

#### AC-3: `score_name_match_pre_lower` missing "query contains name" tier — asymmetric with `NameMatcher::score`
- **Difficulty:** easy
- **Location:** src/store/helpers.rs:774-787
- **Description:** `score_name_match_pre_lower` (used by `search_by_name`) has three tiers: exact (1.0), prefix (0.9), contains (0.7), else 0.0. It only checks if `name contains query`, not if `query contains name`. By contrast, `NameMatcher::score` (used by `search_filtered` hybrid scoring) has a "query contains name" tier at 0.6. This means searching by name "parseConfigFile" will score the function "parse" at 0.0, even though "parse" is a meaningful substring of the query. For `search_by_name` this matters less (FTS handles prefix/substring), but the asymmetry between the two scoring functions can cause surprising rank differences between `--name-only` and hybrid search for the same query.
- **Suggested fix:** Add a `query_lower.contains(name_lower)` check returning 0.6 between the `name_lower.contains(query_lower)` check (0.7) and the final 0.0 fallback. This aligns with `NameMatcher::score`'s tier structure.

#### AC-4: `bfs_shortest_path` in trace has no node cap — unbounded memory on dense graphs
- **Difficulty:** easy
- **Location:** src/cli/commands/trace.rs:198-238
- **Description:** The `bfs_shortest_path` function used by `cqs trace` has no equivalent of `DEFAULT_BFS_MAX_NODES` (10,000) that all three BFS functions in `src/impact/bfs.rs` enforce. On a dense call graph with thousands of functions, the `visited` HashMap and BFS queue can grow to contain every node in the graph. While `max_depth` provides some bound, a depth of 10 on a graph where each function calls 20 others means 20^10 potential queue entries (bounded by unique nodes, but still potentially the entire graph). The impact BFS functions learned this lesson and added the cap after a production incident.
- **Suggested fix:** Add a `const BFS_MAX_NODES: usize = 10_000;` check in the while loop, breaking with a warning when `visited.len() >= BFS_MAX_NODES`. Reuse the `DEFAULT_BFS_MAX_NODES` constant from `src/impact/bfs.rs` (make it `pub(crate)` if needed).

#### AC-5: `rrf_fuse` double-counts duplicate IDs in semantic list
- **Difficulty:** easy
- **Location:** src/store/search.rs:131-135
- **Description:** `rrf_fuse` iterates over `semantic_ids` with `enumerate()` and accumulates `1/(K+rank+1)` per occurrence using `+=`. If the same ID appears at multiple positions in the semantic list (e.g., due to a bug in the caller or HNSW returning duplicates), it accumulates RRF contribution from each rank position, inflating its score above what any single-appearance ID can achieve. The property test `prop_rrf_scores_bounded` even acknowledges this: "Duplicates in input lists can accumulate extra points." While the current callers (BoundedScoreHeap, sorted candidates) shouldn't produce duplicates, the function itself doesn't enforce uniqueness, making it fragile against future callers or index corruption.
- **Suggested fix:** Either (1) deduplicate inputs before scoring (take the best rank per ID), or (2) use `entry().or_insert()` without `+=` and take `max` instead: `let entry = scores.entry(id).or_insert(0.0); *entry = entry.max(contribution);`. Option (1) is cleaner but changes semantics (an ID appearing in both semantic and FTS lists should accumulate). The real fix is to deduplicate within each list independently, then accumulate across lists.

#### AC-6: `token_pack` first-item override can exceed budget by orders of magnitude
- **Difficulty:** easy
- **Location:** src/cli/commands/mod.rs:148-158
- **Description:** `token_pack` has a special case: when no items have been kept yet (`!kept_any`), it includes the first item (by score) even if it exceeds the budget, with a debug log. This is intentional for user-facing search (always show at least one result), but the override has no upper bound. If the highest-scoring result is a 50,000-token file and the budget is 100 tokens, the output reports `tokens_used=50000` against `budget=100`. Callers like `task` waterfall budgeting trust `tokens_used` to compute remaining budget for subsequent sections — a massive overshoot in one section collapses the budget for all downstream sections, potentially allocating zero tokens to impact analysis and placement suggestions. The `index_pack` variant correctly returns empty for budget=0, but `token_pack` can blow past any budget on the first item.
- **Suggested fix:** Add a maximum overshoot factor: `if !kept_any && tokens <= budget * 3 { include } else { break }`. This preserves the "always show one result" behavior for reasonable items while preventing pathological overshoots. Alternatively, the waterfall caller in `task` should use `min(used, budget)` when computing remaining budget downstream.

## Extensibility

#### EX-1: Adding a new CLI command requires changes in 5+ files with no shared registration mechanism
- **Difficulty:** hard
- **Location:** src/cli/definitions.rs, src/cli/dispatch.rs, src/cli/batch/commands.rs, src/cli/batch/handlers/, src/cli/commands/
- **Description:** Adding a new CLI command (e.g., `cqs foo`) requires changes in at least 5 files: (1) `definitions.rs` -- add `Commands::Foo` variant with clap annotations, (2) `dispatch.rs` -- add match arm + import, (3) `commands/foo.rs` + `commands/mod.rs` -- implement `cmd_foo` + re-export, (4) `batch/commands.rs` -- add `BatchCmd::Foo` variant (duplicate clap definition), (5) `batch/handlers/` -- implement `dispatch_foo`. The batch command enum (`BatchCmd`, 290 variants at 747 lines) duplicates most of `Commands` (769 lines) with near-identical clap annotations. Some commands share argument structs via `src/cli/args.rs` (GatherArgs, ImpactArgs, etc.), but 20+ commands define their args inline in both enums. The `plan.rs` "Add CLI Command" template (if it existed) would need 6 entries.
- **Suggested fix:** Short term: extract all remaining inline argument structs into `args.rs` and use `#[command(flatten)]` in both `Commands` and `BatchCmd`, reducing the per-command delta to a one-liner in each enum. Long term: consider a macro or trait-based registration that generates both CLI and batch variants from a single definition. The `define_languages!` macro is a precedent for this pattern.

#### EX-2: HNSW build parameters (M, max_layer, ef_construction) are compile-time constants with no config or env override
- **Difficulty:** medium
- **Location:** src/hnsw/mod.rs:56-58
- **Description:** `MAX_NB_CONNECTION` (M=24), `MAX_LAYER` (16), and `EF_CONSTRUCTION` (200) are compile-time constants used in all 4 build paths (`build`, `build_batched`, `build_incremental`, `build_batched_with_dim`). The code comments explicitly describe different optimal values for different workload sizes ("Smaller codebases: M=16, ef_construction=100; Larger codebases: M=32, ef_construction=400") but provides no way to select them. In contrast, `ef_search` is already configurable via `config.ef_search` and the `set_ef_search()` method. The build parameters affect index quality and build time -- M=24 with ef_construction=200 is expensive for a 2k-chunk toy project and possibly insufficient for a 500k-chunk monorepo.
- **Suggested fix:** Add `hnsw_m`, `hnsw_max_layer`, and `hnsw_ef_construction` fields to `Config` (same pattern as existing `ef_search`). Pass them through to `HnswIndex::build*` methods. Fall back to current constants when unset. Environment variable overrides (`CQS_HNSW_M`, etc.) for one-off tuning. Validate ranges in `Config::validate()`.

#### EX-3: Windowing constants (MAX_TOKENS_PER_WINDOW, WINDOW_OVERLAP_TOKENS) are hardcoded to E5-base-v2's 512-token limit
- **Difficulty:** medium
- **Location:** src/cli/pipeline.rs:28-31
- **Description:** `MAX_TOKENS_PER_WINDOW` (480) and `WINDOW_OVERLAP_TOKENS` (64) are hardcoded constants with the comment "E5-base-v2 has 512 token limit; we use 480 for safety." Since v1.9.0, BGE-large (8192 max tokens) is the default model. Since v1.13.0, v9-200k (also 512 token limit) is a preset. The windowing constants should derive from the active model's context window, not be pinned to the legacy default. With BGE-large, the 480-token window is 17x smaller than necessary, causing unnecessary chunk splitting and embedding overhead. A 3000-token function gets split into 7 windows instead of 1.
- **Suggested fix:** Add a `max_tokens` field to `ModelConfig` (BGE-large: 8192, E5-base: 512, v9-200k: 512). Derive `MAX_TOKENS_PER_WINDOW` from `model_config.max_tokens - 32` (safety margin) and `WINDOW_OVERLAP_TOKENS` from `max_tokens / 8`. Pass the model config into `apply_windowing()` instead of using module-level constants. The compile-time `assert!(MAX_TOKENS_PER_WINDOW <= 512)` test would become a runtime check against the active model.

#### EX-4: `DEFAULT_NAME_BOOST`, `DEFAULT_LIMIT`, `DEFAULT_THRESHOLD` scattered across 3 modules with no single source
- **Difficulty:** easy
- **Location:** src/store/helpers.rs:614, src/cli/config.rs:18-19, src/cli/definitions.rs:136,146
- **Description:** Search defaults are defined in three places that must stay in sync: `DEFAULT_NAME_BOOST` (0.2) in `store/helpers.rs`, `DEFAULT_LIMIT` (5) and `DEFAULT_THRESHOLD` (0.3) in `cli/config.rs`, and the `default_value` annotations in `cli/definitions.rs` (`limit: "5"`, `threshold: "0.3"`, `name_boost: "0.2"`). The clap annotations use string literals (`"5"`, `"0.3"`, `"0.2"`) that cannot reference the Rust constants. If someone changes `DEFAULT_LIMIT` to 10 in config.rs, the clap `--help` output still says "default: 5" and the CLI still defaults to 5 (clap parses the string literal, ignoring the constant). The `Config::load` path uses the constants, creating a split: `cqs "query"` gets the clap default (5), while `.cqs.toml` with no `limit` field gets the Config default (also 5, but from a different constant).
- **Suggested fix:** Define all search defaults in a single `defaults` module. Use `clap::builder::IntoResettable` or a custom value_parser to derive clap defaults from the constants. Alternatively, accept the split but add compile-time assertions: `const _: () = assert!(DEFAULT_LIMIT == 5);` next to the clap annotation, so a constant change without a clap update causes a build failure.

#### EX-5: Watch mode constants (HNSW_REBUILD_THRESHOLD=100, MAX_PENDING_FILES=10000) not configurable via config or env
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:38,41
- **Description:** `HNSW_REBUILD_THRESHOLD` (100) controls how frequently watch mode triggers a full HNSW rebuild to clean orphan vectors. `MAX_PENDING_FILES` (10000) caps the pending file buffer. Both are hardcoded with no config or env override. For large monorepos with frequent file changes, 100 incremental inserts before rebuild may be too aggressive (rebuilding a 50k-chunk HNSW takes 10+ seconds). For small projects, 10000 pending files is meaninglessly large. The `CQS_CAGRA_THRESHOLD` env-var pattern (src/cli/mod.rs:108) already exists for the CAGRA threshold -- watch constants should follow the same pattern.
- **Suggested fix:** Add `CQS_HNSW_REBUILD_THRESHOLD` and `CQS_MAX_PENDING_FILES` env var overrides using the same `std::env::var().ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT)` pattern as CAGRA threshold. Or add `watch.hnsw_rebuild_threshold` and `watch.max_pending_files` to `Config`. The env-var approach is simpler and consistent with the existing pattern.

#### EX-6: Markdown parser section size limits (MIN_SECTION_LINES=30, MAX_SECTION_LINES=150) are hardcoded with no per-project tuning
- **Difficulty:** easy
- **Location:** src/parser/markdown/mod.rs:35,37
- **Description:** `MIN_SECTION_LINES` (30) and `MAX_SECTION_LINES` (150) control how markdown documents are chunked for embedding. Sections smaller than 30 lines are merged with the next section; sections larger than 150 lines are split at sub-headings. These values work well for typical documentation but are wrong for two common cases: (1) API reference docs with many small sections (5-10 lines each) that get incorrectly merged, losing granularity; (2) long-form prose (design docs, RFCs) where 150 lines is too small, splitting coherent arguments across chunks. Since cqs indexes markdown READMEs, CHANGELOGs, and reference docs alongside code, the chunking strategy significantly affects search quality for these files.
- **Suggested fix:** Add `markdown.min_section_lines` and `markdown.max_section_lines` to Config. Pass them through to `parse_markdown()` instead of using module-level constants. Default to current values (30, 150). This is a 4-field addition to Config and a 2-parameter addition to the parse function.

## Robustness

#### RB-1: `cmd_query` panics on multi-byte UTF-8 query at byte position 200
- **Difficulty:** easy
- **Location:** src/cli/commands/query.rs:42
- **Description:** The tracing preview truncates queries with `&query[..200]`, a direct byte-index slice on user input. If a user passes a query containing multi-byte characters (CJK text, emoji, accented characters) where byte 200 falls in the middle of a multi-byte sequence, this panics with `byte index 200 is not a char boundary`. The embedder's own truncation (line 460-465 in `embedder/mod.rs`) correctly uses `is_char_boundary()` to find a safe truncation point, but `cmd_query` does not. This is reachable from any CLI query or batch search command with a long non-ASCII query string.
- **Suggested fix:** Use `query.floor_char_boundary(200)` (available since Rust 1.73, MSRV is 1.93): `&query[..query.floor_char_boundary(200)]`. This is the same pattern already used at `cli/commands/task.rs:765` for note text truncation. One-line change.

#### RB-2: `rerank_with_passages` uses `assert_eq!` for input validation — panics on mismatched lengths
- **Difficulty:** easy
- **Location:** src/reranker.rs:127-130
- **Description:** The public method `rerank_with_passages(query, results, passages, limit)` uses `assert_eq!(results.len(), passages.len(), ...)` to validate that `results` and `passages` have the same length. This panics instead of returning an error. While the internal caller `rerank()` (line 100) constructs `passages` from `results` so they always match, `rerank_with_passages` is `pub` and accessible to any future caller or agent-invoked code path. The function already returns `Result<(), RerankerError>`, so a proper error return is trivial. The `assert_eq!` in production code violates the project convention of "no `unwrap()` except in tests."
- **Suggested fix:** Replace the `assert_eq!` with an early return: `if results.len() != passages.len() { return Err(RerankerError::Inference(format!("passages length {} != results length {}", passages.len(), results.len()))); }`. One-line change.

#### RB-3: `Language::grammar()` panics on grammar-less languages — still exists as dead code
- **Difficulty:** easy
- **Location:** src/language/mod.rs:860-864
- **Description:** `grammar()` uses `unwrap_or_else(|| panic!(...))` for languages without a tree-sitter grammar (Markdown, plain text). The v1.12.0 audit flagged this as RB-31 (P2). Since then, all 9+ callers have been migrated to `try_grammar()` which returns `Option`. The panicking `grammar()` method now has zero production callers — it's dead code that could be re-introduced by a future caller who doesn't know about `try_grammar()`. The method is still `pub` and discoverable in IDE autocomplete.
- **Suggested fix:** Either (a) delete `grammar()` entirely since it has zero callers, or (b) add `#[deprecated(note = "Use try_grammar() instead")]` to prevent new adoption. Option (a) is cleaner — dead code should be removed per project conventions.

#### RB-4: `Cli::model_config()` panics if called before dispatch resolves it
- **Difficulty:** easy
- **Location:** src/cli/definitions.rs:237-239
- **Description:** `model_config()` calls `.expect("ModelConfig not resolved — call resolve_model() first")` on `Option<ModelConfig>`. Currently, `dispatch()` (line 34) sets `resolved_model` before any command runs, making the `expect` safe in practice. However, this is a temporal coupling: any code that calls `model_config()` outside the dispatch path (e.g., a new entry point, a test harness, or agent-invoked code) will panic with an opaque message. The function has 20+ callers in `src/cli/commands/` — all are safe today, but the design is fragile. This is a variant of the "assert in production" anti-pattern where correctness depends on call ordering rather than types.
- **Suggested fix:** Return `Result<&ModelConfig, anyhow::Error>` instead of panicking: `self.resolved_model.as_ref().ok_or_else(|| anyhow::anyhow!("ModelConfig not resolved — call resolve_model() first"))`. All 20+ callers already return `Result`, so appending `?` is mechanical. Alternatively, accept the `expect` but add a `debug_assert!` comment explaining the invariant.

#### RB-5: `embed_batched` panics if ONNX output shape has unexpected dimensions
- **Difficulty:** medium
- **Location:** src/embedder/mod.rs:640, 657
- **Description:** After ONNX inference, the code extracts shape dimensions with `shape[2] as usize` (line 640) and checks `shape[0] as usize != batch_size` (line 657). If the ONNX model returns an output tensor with fewer than 3 dimensions, `shape[2]` is an out-of-bounds index panic. This is reachable when using `--model` with a custom ONNX model that doesn't produce the expected `[batch, seq_len, hidden_dim]` output shape (e.g., a sentence-level model that outputs `[batch, hidden_dim]` directly). The ONNX model is user-configurable via `[embedding]` config or `CQS_EMBEDDING_MODEL` env var.
- **Suggested fix:** Validate shape length before indexing: `if shape.len() < 3 { return Err(EmbedderError::Inference(format!("Expected 3D output [batch, seq, dim], got {}D", shape.len()))); }`. Add similar guard for `shape[0]` check. Three lines added.

#### RB-6: `enrichment_hash` inputs can produce different hashes for semantically identical enrichments
- **Difficulty:** easy
- **Location:** src/cli/enrichment.rs:256-276
- **Description:** The `enrichment_hash` function builds a hash from `format!("nl:{nl}")` where `nl` is the enriched natural language description. If the LLM provider changes whitespace formatting (trailing spaces, leading newlines) between API calls for the same function, the hash changes, causing unnecessary re-embedding on the next `cqs index --enrich` run. The function also includes `hyde` and `doc` fields but doesn't normalize whitespace on `nl`. The `nl` content comes directly from LLM API response text. This means provider-side formatting changes cause cache invalidation and wasted compute — not a crash, but a robustness issue for the enrichment pipeline.
- **Suggested fix:** Trim and normalize whitespace in `nl` before hashing: `let nl = nl.split_whitespace().collect::<Vec<_>>().join(" ");`. This makes the hash stable across whitespace-only formatting changes. Same treatment for `doc` and `hyde` inputs.

## Platform Behavior

#### PB-1: HNSW save lock uses `File::create` (truncates) while load lock uses `OpenOptions` (no truncate)
- **Difficulty:** easy
- **Location:** src/hnsw/persist.rs:151
- **Description:** The HNSW save path acquires the exclusive lock via `std::fs::File::create(&lock_path)?` (line 151), which creates-and-truncates. The load path on line 399 uses `OpenOptions::new().create(true).truncate(false).open(&lock_path)?`. The index lock in `src/cli/files.rs:59-82` also carefully avoids truncation with a comment explaining why: "Does NOT truncate -- another process's PID remains readable until we acquire the lock and overwrite it." The HNSW save truncation is harmless on Unix (the lock is on the file descriptor, not the content), but on Windows, `File::create` can fail with a sharing violation if another process already holds the file open for shared reading. A concurrent `count_vectors()` call (line 553, which opens the .ids file, not the lock) won't hit this, but a concurrent `load()` holding the lock file in shared mode (line 399-405) would see the content zeroed under it. The inconsistency also makes the code harder to reason about.
- **Suggested fix:** Change line 151 from `std::fs::File::create(&lock_path)?` to `std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&lock_path)?` -- matching the load path and the index lock pattern. The lock file has no meaningful content (unlike the index lock's PID), so the behavior is identical, just more robust and consistent.

#### PB-2: `collect_events` compares non-canonicalized deleted-file paths against canonicalized `cfg.cqs_dir` and `cfg.notes_path`
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:364-377
- **Description:** When a file is deleted, `collect_events` (line 362-408) intentionally skips `dunce::canonicalize` because the file no longer exists (PB-26 comment on line 364). The raw path is then compared against `cfg.cqs_dir` (canonicalized on line 220) via `path.starts_with(cfg.cqs_dir)` (line 372) and against `cfg.notes_path` (canonicalized on line 224) via `path == cfg.notes_path` (line 377). If the filesystem event delivers a path with symlinks or non-canonical components (e.g., `//` double slashes, or `./` prefixes), the raw path won't match the canonical reference paths. On WSL with `/mnt/c/` mounts, notify events sometimes report paths differently from canonical form. The consequence: a deleted `.cqs/` file could slip through the filter and be added to `pending_files`, or a `notes.toml` deletion could fail to set `pending_notes`. Both are low-impact (deleted .cqs files would just fail to index, notes deletion is rare), but the code's intent is to filter them and the filter is unreliable.
- **Suggested fix:** For the `.cqs` directory check, compare against both the canonical and the raw `cqs_dir` path. For deleted files, also try `path.canonicalize()` on the parent directory (which still exists) and reconstruct: `path.parent().and_then(|p| dunce::canonicalize(p).ok()).map(|p| p.join(path.file_name().unwrap()))`. Alternatively, since the fallback path (line 368-369) is just `path.clone()`, compare both `path.starts_with(cfg.cqs_dir)` AND `path.starts_with(&original_raw_cqs_dir)` where `original_raw_cqs_dir` is the pre-canonicalized value.

#### PB-3: `find_7z` hardcodes Windows path with backslashes, not tested on Windows
- **Difficulty:** easy
- **Location:** src/convert/chm.rs:193
- **Description:** `find_7z()` includes `r"C:\Program Files\7-Zip\7z.exe"` as a fallback candidate. This is passed to `Command::new(name)` which works on Windows because Windows accepts both slash directions. However, this candidate is only useful on native Windows -- on WSL, the Windows `C:\` drive is at `/mnt/c/`. A WSL user with 7-Zip installed on Windows but not `p7zip` in Linux would get "7z not found" even though the tool exists at `/mnt/c/Program Files/7-Zip/7z.exe`. Since cqs runs on WSL as a primary platform (per CLAUDE.md), this is a real gap. The install hint also doesn't mention WSL: it says "Install: `sudo apt install p7zip-full` (Linux), `brew install p7zip` (macOS), or 7-Zip (Windows)" -- a WSL user should get the apt hint, but the Windows path suggests they could use the Windows binary (they can't, via this code path).
- **Suggested fix:** Add the WSL-translated path `/mnt/c/Program Files/7-Zip/7z.exe` to the candidates list when `cqs::config::is_wsl()` is true. Or remove the Windows path entirely and let Windows users have 7z on PATH (more conventional). The error message is already adequate for Linux users.

#### PB-4: `find_python` in `export_model.rs` and `pdf.rs` are duplicate implementations with no WSL awareness
- **Difficulty:** easy
- **Location:** src/cli/commands/export_model.rs:8-25, src/convert/pdf.rs:288-309
- **Description:** Two identical `find_python()` functions exist: one in `export_model.rs` (line 8) and one in `pdf.rs` (line 292). Both try `python3`, `python`, `py` in order. Neither checks for `python3.exe` or the WSL interop path. On WSL, if a user has Python installed only on Windows (common for data science setups), these functions won't find it. The `py` launcher exists on Windows but not on WSL -- `py.exe` would work via WSL interop but is not tried. The duplication is also a maintenance hazard: if one gets a bug fix (e.g., a timeout on the `--version` check), the other won't.
- **Suggested fix:** Extract a shared `find_python()` into a common utility module (e.g., `src/convert/mod.rs` or `src/util.rs`). Both call sites import the shared function. WSL awareness is optional -- `apt install python3` is the right answer for WSL users, and WSL interop adds latency.

#### PB-5: `prune_missing` and `check_origins_stale` macOS case-fold uses `to_lowercase()` which diverges from APFS Unicode normalization
- **Difficulty:** medium
- **Location:** src/store/chunks/staleness.rs:49-55, src/store/chunks/staleness.rs:134-144
- **Description:** Already triaged as PB-34 in v1.12.0. Noting here for completeness: the code uses `to_lowercase()` for case-insensitive comparison on macOS (lines 49-55, 134-140), but APFS uses ICU case-folding which differs from Rust's `to_lowercase()` for certain non-ASCII characters (e.g., German sharp-s, Turkish dotless-i). `prune_missing()` (line 49) and `prune_all()` (line 134) have the same pattern -- a file named with non-ASCII characters on APFS could be falsely pruned because the Rust comparison doesn't match the filesystem's view. The `check_origins_stale()` function on line 356 does NOT have the macOS case-fold handling -- it compares paths with `==` -- so there's also an inconsistency within the same module.
- **Suggested fix:** Skip -- already triaged as PB-34. The `check_origins_stale` inconsistency (no case-fold there) is new though: either add the same `#[cfg(target_os = "macos")]` block to `check_origins_stale`, or document why it's intentionally absent. Since `check_origins_stale` checks mtime-based staleness (file still exists, content may have changed) rather than existence, the case-fold mismatch there means it could report a file as stale when it's actually current (or vice versa) on a macOS case-insensitive volume.

#### PB-6: `ort_runtime_search_dir` reads `/proc/self/cmdline` without fallback -- fails on macOS and FreeBSD
- **Difficulty:** easy
- **Location:** src/embedder/provider.rs:73-88
- **Description:** `ort_runtime_search_dir()` reads `/proc/self/cmdline` to determine `argv[0]` and compute the directory ORT will search for provider libraries. This function is gated with `#[cfg(target_os = "linux")]` (line 72), so it correctly won't compile on macOS. However, the fallback `ensure_ort_provider_libs()` on non-Linux (line 200-205) is a complete no-op with a comment "Windows and other platforms find CUDA/TensorRT provider libraries via PATH." On macOS with CUDA (via external eGPU or future Apple silicon support), the same `dlopen` search-path issue could occur -- ORT's `GetRuntimePath()` on macOS uses `_NSGetExecutablePath` instead of `/proc`, but the provider libraries still need to be findable. The no-op means macOS users with CUDA would silently fall back to CPU with no indication that the providers aren't discoverable. This is low priority since macOS+CUDA is rare, but the no-op should at least log that it's skipping provider setup.
- **Suggested fix:** Add `tracing::debug!("Provider library setup not implemented for this platform")` to the non-Linux fallback on line 201-205, so users see why GPU isn't activating. If macOS CUDA support becomes relevant, implement using `std::env::current_exe()` (which uses `_NSGetExecutablePath` internally) instead of `/proc`.

#### PB-7: `doc_writer::rewriter::rewrite_file` locks the source file itself instead of a separate lock file
- **Difficulty:** medium
- **Location:** src/doc_writer/rewriter.rs:246-247
- **Description:** `rewrite_file()` acquires an exclusive lock on the source file itself: `let lock_file = std::fs::File::open(path)?; lock_file.lock()?;` (lines 246-247). Then it reads the file, modifies content, and writes it back. The notes system (`src/note.rs:146-158, 217-238`) uses a separate `.lock` file for locking and operates on the data file independently, with a detailed comment explaining why (line 138-141): "If we locked the data file itself, a concurrent writer's atomic rename would orphan our lock onto the old inode, letting a third process read stale data." The same problem applies to `rewrite_file` -- if another process renames the file (e.g., an editor's save-swap pattern), the lock is on the old inode. On Windows (native, not WSL), this is worse: Windows mandatory locks prevent other processes from reading the file while the exclusive lock is held, so `--improve-docs` could block IDE file watchers or git operations until the write completes. On Unix the lock is advisory so it only protects against other cqs processes.
- **Suggested fix:** Follow the notes pattern: lock a separate `{path}.cqs-lock` file instead of the source file. Open with `OpenOptions::new().create(true).truncate(false)`, acquire exclusive lock, then operate on the source file. This avoids the inode-orphaning issue and the Windows mandatory-lock interference. Clean up the lock file after completion (or leave it as a 0-byte sentinel -- the notes code leaves its lock file).

## Scaling & Hardcoded Limits

Constants that should scale with model configuration, corpus size, or hardware.

### Model-Dependent Constants

#### SHL-1: MAX_TOKENS_PER_WINDOW=480 hardcoded to E5-base's 512-token limit
- **Difficulty:** medium
- **Location:** src/cli/pipeline.rs:30
- **Description:** `MAX_TOKENS_PER_WINDOW` is pinned to 480 (512 minus safety margin) with a compile-time assertion `assert!(MAX_TOKENS_PER_WINDOW <= 512)` at line 1121. The comment explicitly says "E5-base-v2 has 512 token limit" but the default model is now BGE-large (also 512 tokens). If a future model supports longer contexts (e.g., 8192 tokens for jina-embeddings-v3), this constant silently truncates input to 6% of capacity. Every chunk longer than ~480 tokens gets split into overlapping windows, producing more chunks and diluting search quality with redundant partial-content embeddings.
- **Should depend on:** `ModelConfig::max_seq_length` (already available, already plumbed to the Embedder).
- **Impact:** Silent search quality degradation. Long functions get split into 480-token windows even when the model can handle the full content in one pass. More windows = more embeddings = larger index = slower search, all for no quality benefit.
- **Suggested fix:** Replace the constant with `model_config.max_seq_length - 32` (safety margin). The compile-time assertion should be removed; the runtime check already exists in the Embedder's tokenizer truncation. Pass the model config into `apply_windowing()` to compute the window size dynamically.

#### SHL-2: Reranker max_length=512 hardcoded, ignores model capability
- **Difficulty:** easy
- **Location:** src/reranker.rs:86
- **Description:** `Reranker::new()` hardcodes `max_length: 512`. The cross-encoder model (ms-marco-MiniLM-L-6-v2) does have a 512-token limit, but the reranker already supports swapping models via `CQS_RERANKER_MODEL` env var. If a user sets a cross-encoder with a longer context (e.g., BAAI/bge-reranker-large supports 8192), their passages are silently truncated to 512 tokens.
- **Should depend on:** Model-specific max_length, either from config or auto-detected from the model's tokenizer config.
- **Impact:** Suboptimal reranking. Long code chunks get truncated before the cross-encoder scores them, potentially cutting off the most relevant part of the function body.
- **Suggested fix:** Add `CQS_RERANKER_MAX_LENGTH` env var override, defaulting to 512. Or parse `tokenizer_config.json` from the model repo to auto-detect.

#### SHL-3: NL description char budget assumes 512-token model
- **Difficulty:** easy
- **Location:** src/nl/mod.rs:189-190,199
- **Description:** The NL description generator for markdown sections hardcodes `.take(1800)` characters with a comment: "Embedding models handle ~512 tokens (~2000 chars)." The 4:1 char-to-token ratio is a rough heuristic. For a model with 8192-token context, this wastes 94% of the input capacity. For code (which tokenizes less efficiently than English prose), the ratio is closer to 3:1, meaning the actual token count is ~600 -- already over the assumed 512.
- **Should depend on:** `ModelConfig::max_seq_length` converted to an approximate char budget.
- **Impact:** Markdown section embeddings lose content. Long documentation sections get truncated at 1800 chars, which may cut off key information that would improve retrieval.
- **Suggested fix:** Compute char budget as `model_config.max_seq_length * 3` (conservative code ratio) or `* 4` (English text ratio), depending on chunk type. Pass through from the pipeline or make it a function of the active model config.

#### SHL-4: diff.rs memory comment references "768 dims" instead of runtime dim
- **Difficulty:** trivial
- **Location:** src/diff.rs:157
- **Description:** Comment says "For 20k pairs at ~12 bytes/dim * 768 dims" but the default model is now BGE-large (1024 dims). The code itself is correct (uses dynamic dim from embeddings), but the stale comment could mislead someone sizing batch parameters. At 1024 dims, each batch is ~12 MB, not ~9 MB.
- **Should depend on:** N/A (comment-only fix).
- **Impact:** None at runtime; misleading for maintainers.
- **Suggested fix:** Update comment to say "12 bytes/dim * dim" or reference the actual default (1024).

#### SHL-5: embedder output comment says "[batch, seq_len, 768]" regardless of model
- **Difficulty:** trivial
- **Location:** src/embedder/mod.rs:622,631
- **Description:** Comments on lines 622 and 631 say "shape [batch, seq_len, 768]" but the actual dimension depends on the model (1024 for BGE-large). The code correctly uses the dynamic shape from the tensor, so this is comment-only.
- **Should depend on:** N/A (comment-only fix).
- **Impact:** None at runtime; misleading for developers.
- **Suggested fix:** Change "768" to "dim" in the comments (e.g., "[batch, seq_len, dim]").

### HNSW Parameters

#### SHL-6: HNSW parameters (M=24, ef_construction=200, ef_search=100) hardcoded for 10k-100k chunks
- **Difficulty:** medium
- **Location:** src/hnsw/mod.rs:56-61
- **Description:** The HNSW tuning parameters are optimized for "10k-100k chunks" per the comment. The comment even documents what to use for different sizes: "Smaller codebases (<5k): M=16" and "Larger codebases (>100k): M=32, ef_construction=400, ef_search=200." But the code ignores its own advice -- these are compile-time constants used everywhere. A 500-chunk project wastes memory on M=24 connectivity, while a 200k-chunk project gets degraded recall from insufficient M and ef_search. The CAGRA GPU index has its own separately tuned parameters, so CPU and GPU paths already diverge.
- **Should depend on:** Index size at build time (known from `chunk_count`). The comment documents the exact scaling rules.
- **Impact:** Suboptimal recall for large indexes (>100k). Wasted memory/build time for small indexes (<5k). For a 200k-chunk monorepo, ef_search=100 may miss relevant results that ef_search=200 would find.
- **Suggested fix:** Make `build_hnsw_params(chunk_count: usize)` return `(M, max_layer, ef_construction)` and `search_ef(chunk_count: usize)` return the runtime ef_search. Use the thresholds already documented in the comment. Alternatively, add env var overrides (`CQS_HNSW_M`, `CQS_HNSW_EF_SEARCH`).

### Corpus-Size-Dependent Constants

#### SHL-7: MAX_CONTRASTIVE_CHUNKS=15000 is an OOM guard that should scale with available RAM
- **Difficulty:** medium
- **Location:** src/llm/summary.rs:158
- **Description:** The contrastive summary feature builds an N*N similarity matrix, costing N*N*4 bytes. The cap at 15,000 chunks limits this to ~900MB. On a machine with 128GB RAM, the cap could be 50k+ chunks (~10GB). On a 16GB laptop, even 15k might be too aggressive. The DS-21 comment acknowledges the tradeoff but picks a single fixed number.
- **Should depend on:** Available system memory, or at least be configurable via env var.
- **Impact:** Large codebases (>15k callable chunks) get no contrastive summaries at all, falling back to non-contrastive. This affects summary quality for exactly the projects that need it most.
- **Suggested fix:** Add `CQS_MAX_CONTRASTIVE_CHUNKS` env var. Optionally, auto-detect available RAM and compute `sqrt(available_bytes / 4)` as the default cap.

#### SHL-8: DEFAULT_MAX_EXPANDED_NODES=200 in gather limits BFS exploration regardless of codebase size
- **Difficulty:** easy
- **Location:** src/gather.rs:23
- **Description:** `gather` does BFS expansion from seed search results, capped at 200 nodes. For a small project (500 chunks), 200 nodes = 40% coverage -- good. For a large monorepo (200k chunks), 200 nodes = 0.1% -- may miss relevant code 3+ hops from seed results. The `--max-nodes` flag exists on the CLI but the library default is fixed.
- **Should depend on:** Nothing urgent -- the default is reasonable and the CLI override exists. But the default could scale mildly with corpus size.
- **Impact:** Suboptimal context assembly for large codebases. Gather may miss relevant code that requires deeper BFS traversal.
- **Suggested fix:** Low priority. The CLI override (`--max-nodes`) already exists. Consider adjusting the library default to `min(500, chunk_count / 10)` if corpus size is available at call time.

#### SHL-9: DEFAULT_BFS_MAX_NODES=10000 in impact BFS has no relation to graph density
- **Difficulty:** easy
- **Location:** src/impact/bfs.rs:9
- **Description:** BFS traversal in impact analysis caps at 10,000 nodes regardless of the call graph's actual size or density. In a highly connected codebase, BFS may hit 10,000 nodes quickly without reaching the actual impact boundary.
- **Should depend on:** Call graph edge count or node count.
- **Impact:** For dense call graphs, may miss transitive callers beyond the 10k frontier. Mostly a performance concern -- the cap prevents runaway BFS.
- **Suggested fix:** Low priority. The current cap is a safety valve. Consider `min(10_000, graph_node_count)` to avoid allocating for nodes that don't exist.

### Hardware-Dependent Constants

#### SHL-10: MAX_CAGRA_CPU_BYTES=2GB ignores actual available RAM
- **Difficulty:** easy
- **Location:** src/cagra.rs:451
- **Description:** The CAGRA GPU index builder refuses to load datasets requiring >2GB of CPU staging memory. On a machine with 48GB GPU RAM and 128GB system RAM, this artificially limits GPU indexing to ~500k chunks (at 1024 dims). The 2GB limit was likely set for safety on low-memory machines, but it prevents GPU acceleration for the exact workloads that benefit most.
- **Should depend on:** Available system memory (or a configurable fraction thereof).
- **Impact:** Large codebases fall back to CPU-only HNSW indexing even when a capable GPU is available. GPU CAGRA indexing is 5-10x faster than CPU HNSW for large datasets.
- **Suggested fix:** Add `CQS_CAGRA_MAX_CPU_BYTES` env var. Default to `min(available_ram * 0.25, 8GB)` or keep the 2GB default on systems where available RAM can't be detected.

#### SHL-11: Rayon thread pool hardcoded to 4 threads for reference/project loading
- **Difficulty:** easy
- **Location:** src/reference.rs:106, src/project.rs:220
- **Description:** Reference index loading and cross-project search both hardcode `num_threads(4)`. On a 32-core workstation, this underutilizes available parallelism. On a 2-core CI runner, 4 threads may cause contention. The RM-25/RM-29 comments note each thread loads ~250MB (Store + HNSW), so the limit is about memory, not CPU.
- **Should depend on:** `min(available_cores, ref_count, memory_budget / per_ref_memory)`.
- **Impact:** Slightly slower reference loading on high-core-count machines. Negligible for most workloads (I/O-bound).
- **Suggested fix:** Low priority. Use `CQS_REF_THREADS` env var, default to `min(num_cpus::get(), 8)`.

#### SHL-12: EMBED_BATCH_SIZE=64 fixed regardless of GPU VRAM
- **Difficulty:** medium
- **Location:** src/cli/pipeline.rs:37
- **Description:** The embedding batch size is hardcoded at 64. On an A6000 (48GB VRAM), batch size 64 with BGE-large uses ~200MB -- a fraction of available memory. Batch size 256+ would improve GPU utilization. On an 8GB GPU, 64 might be too large for very long sequences. The comment notes a previous crash at this size, suggesting the optimal value is hardware-dependent.
- **Should depend on:** GPU VRAM and model dimensions.
- **Impact:** Underutilized GPU. Doubling batch size could halve indexing time for the embedding stage.
- **Suggested fix:** Add `CQS_EMBED_BATCH_SIZE` env var override. Consider auto-tuning: start with batch_size=128 on GPU, halve on OOM retry.

### Scoring Constants

#### SHL-13: RRF K=60 from web search paper, not tuned for code search
- **Difficulty:** trivial
- **Location:** src/store/search.rs:126
- **Description:** The RRF fusion constant K=60 comes from the original RRF paper (Cormack et al., 2009), tuned for web search corpora with millions of documents. For code search with 10k-100k chunks, the optimal K may differ. The comment correctly cites the origin. Not a bug, but worth noting for future eval work.
- **Should depend on:** Empirical evaluation on cqs eval set.
- **Impact:** Potentially suboptimal fusion. Low priority -- K=60 is a well-studied default.
- **Suggested fix:** No change needed now. Consider `CQS_RRF_K` env var for experimentation when running evals.

#### SHL-14: DEFAULT_THRESHOLD=0.3 not documented as model-specific
- **Difficulty:** trivial
- **Location:** src/cli/config.rs:19
- **Description:** The minimum cosine similarity threshold of 0.3 filters out search results below this score. Different embedding models produce different score distributions -- BGE-large tends to produce higher scores than E5-base for the same query-document pair. A threshold calibrated for one model may be too aggressive or too lenient for another.
- **Should depend on:** Model-specific score distribution.
- **Impact:** Threshold too high = missing relevant results. Threshold too low = noise. Both are CLI-overridable (`--threshold`), so the defaults just need documentation about which model they're calibrated for.
- **Suggested fix:** Add a comment: "Calibrated for BGE-large score distribution; E5-base may need ~0.25."

#### SHL-15: DEFAULT_QUERY_CACHE_SIZE=32 is small for batch/REPL mode
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:235
- **Description:** The query embedding cache holds 32 entries (~128KB total at 1024 dims). In batch mode (`cqs batch`) or REPL (`cqs chat`), queries may repeat across pipeline stages. Total cache at 32 entries is trivially small.
- **Should depend on:** Usage mode. Batch/REPL could use 256+ entries (~1MB).
- **Impact:** Minor. Re-embedding a query takes ~5ms on GPU, so cache misses cost ~0.5s over a 100-query batch session.
- **Suggested fix:** Low priority. Increase default to 128. Or add `CQS_QUERY_CACHE_SIZE` env var.

## Performance

#### PERF-1: `find_test_chunks` cache clones entire `Vec<ChunkSummary>` on every call
- **Difficulty:** easy
- **Location:** src/store/calls/test_map.rs:76-82
- **Description:** `find_test_chunks` caches results in `OnceLock<Vec<ChunkSummary>>` and returns `.clone()` on every hit. `ChunkSummary` contains 7 heap-allocated Strings per instance. With ~1500 test chunks and ~14 callers per session (`impact`, `scout`, `task`, `review`, `suggest`, `health`, `onboard`, `dead`, `test-map`, plus batch handlers), this clones ~1500 * 7 = ~10,500 Strings per call. The notes cache had the same problem (PERF-46 from v1.12.0 triage) and was fixed with `Arc`. The fix is identical: change `OnceLock<Vec<ChunkSummary>>` to `OnceLock<Arc<Vec<ChunkSummary>>>` and return `Arc::clone()` (pointer bump) instead of deep clone.
- **Suggested fix:** Change `test_chunks_cache: OnceLock<Vec<ChunkSummary>>` to `OnceLock<Arc<Vec<ChunkSummary>>>`. Return `Arc<Vec<ChunkSummary>>` from `find_test_chunks`. Update ~14 callers to accept `Arc<Vec<ChunkSummary>>` or `&[ChunkSummary]`.

#### PERF-2: `upsert_fts_conditional` issues 2 SQL statements per changed chunk instead of batching
- **Difficulty:** medium
- **Location:** src/store/chunks/async_helpers.rs:236-275
- **Description:** During chunk upsert, `upsert_fts_conditional` iterates over each changed chunk and issues a DELETE + INSERT into `chunks_fts` individually. On initial indexing of ~11,000 chunks (all "changed"), this is ~22,000 individual SQL round trips within a single transaction. The chunk INSERT itself is already batched at 52 rows per statement via `push_values`, but FTS upsert is still per-row. SQLite transactions amortize fsync cost, but individual statement preparation and execution still costs ~0.1-0.5ms each. For 22K statements that is 2-11 seconds of pure SQL overhead.
- **Suggested fix:** Batch the FTS DELETE with `DELETE FROM chunks_fts WHERE id IN (...)` (groups of 500), then batch the INSERT with `QueryBuilder::push_values` (groups of ~160, since 5 params * 160 = 800 < 999 limit). Only include chunks where `content_changed` is true. This mirrors the pattern already used by `batch_insert_chunks`.

#### PERF-3: Watch mode calls `upsert_type_edges_for_file` per-file in a loop instead of batched version
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:756-759
- **Description:** The watch path iterates over `all_type_refs` and calls `store.upsert_type_edges_for_file(rel_path, chunk_type_refs)` for each file. Each call opens a separate SQLite transaction (`pool.begin()` + commit). A batched alternative already exists: `store.upsert_type_edges_for_files(&all_type_refs)` (line 237 of `src/store/types.rs`) which processes all files in a single transaction. The pipeline stage uses the batch version. When a save-all triggers 50+ files at once in watch mode, this is 50+ unnecessary transaction open/commits.
- **Suggested fix:** Replace the per-file loop with a single call: `store.upsert_type_edges_for_files(&all_type_refs)?;` after converting the `Vec<(PathBuf, Vec<ChunkTypeRefs>)>` to the expected format.

#### PERF-4: Watch `all_calls` filtering scans entire Vec per file instead of pre-grouping
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:733-737
- **Description:** Inside the `for (file, pairs) in &by_file` loop, each file builds a `HashSet<&str>` of its chunk IDs, then linearly scans the entire `all_calls` vec with `.filter(|(id, _)| chunk_ids.contains(id.as_str()))`. If 20 files changed with 200 total calls, this is 20 * 200 = 4000 iterations. The `all_calls` vec also `.cloned()` each matching tuple (allocating `String` + `CallSite`). The fix is to pre-group `all_calls` into a `HashMap<String, Vec<CallSite>>` by chunk_id once before the loop. Each file then looks up its calls in O(1) per chunk.
- **Suggested fix:** Before the `by_file` loop, build `let calls_by_chunk: HashMap<&str, Vec<&CallSite>> = ...` from `all_calls`. In the loop, collect calls for the file's chunks from the map instead of scanning all calls.

#### PERF-5: `upsert_chunks_and_calls` issues individual DELETE per unique caller_id
- **Difficulty:** easy
- **Location:** src/store/chunks/crud.rs:452-460
- **Description:** When upserting calls, the function iterates over unique caller chunk IDs and issues `DELETE FROM calls WHERE caller_id = ?1` for each one individually. With a file containing 50 functions, this is 50 individual DELETE statements. These are within a transaction so fsync is amortized, but statement preparation overhead adds up. The fix is to batch into `DELETE FROM calls WHERE caller_id IN (...)` with groups of 500 (same pattern used everywhere else).
- **Suggested fix:** Collect all unique caller IDs into a vec, batch-delete with `DELETE FROM calls WHERE caller_id IN ({placeholders})` in groups of 500.

#### PERF-6: `finalize_results` clones `ChunkRow` for every search result via `row.clone()`
- **Difficulty:** medium
- **Location:** src/search/query.rs:283
- **Description:** In `finalize_results`, the `filter_map` closure calls `ChunkSummary::from(row.clone())` for each result that passes parent dedup. `ChunkRow` contains 10 heap-allocated Strings (id, origin, language, chunk_type, name, signature, content, doc, content_hash, parent_id, parent_type_name). For a typical search returning 10 results from a pool of ~30 candidates (after `limit * 2` RRF), this is ~20 unnecessary row clones (since the rows are consumed and the original map is dropped). The `rows_map` is a `HashMap<String, ChunkRow>` returned by `fetch_chunks_by_ids_async`. Since it's consumed (not referenced later), the clone could be replaced by removing from the map: `rows_map.remove(&id)` instead of `rows_map.get(&id)` + clone.
- **Suggested fix:** Change `rows_map` to mutable, use `rows_map.remove(&id)` in the `filter_map` closure to move the `ChunkRow` out instead of cloning. This eliminates the clone entirely. Requires changing `final_scored.into_iter().filter_map(|(id, score)| { let row = rows_map.remove(&id)?; ...`.

## Resource Management

#### RM-1: `train_data` dedup HashMap grows unbounded across all commits within a repo
- **Difficulty:** easy
- **Location:** src/train_data/mod.rs:157
- **Description:** The `dedup: HashMap<String, usize>` at line 157 accumulates one entry per unique content_hash across all commits processed for a repo. With `max_commits=0` (unlimited) on a large repo (e.g., 50K commits, each touching ~5 functions), this grows to ~250K entries of `(String, usize)` -- roughly 20MB of heap. The dedup map is never pruned. For multi-repo runs (`config.repos` has multiple entries), each repo gets its own dedup, but the previous repo's BM25 corpus (`bm25_docs`) and commit list (`commits`) also remain live until the outer loop moves on. Peak memory is `bm25_docs(repo_N) + commits(repo_N) + dedup(repo_N)` simultaneously. The BM25 index (`Bm25Index::build`) at line 55 also clones the entire `docs` slice via `docs.to_vec()`, doubling the corpus memory.
- **Suggested fix:** (1) Add a capacity warning when `dedup.len() > 100_000` similar to the `git_log` warning at line 82. (2) For the BM25 clone: change `Bm25Index` to borrow `docs` or take ownership instead of `to_vec()`. (3) Consider a `dedup.shrink_to_fit()` after the commit loop to release excess capacity.

#### RM-2: `Bm25Index::build` clones entire corpus via `docs.to_vec()` -- doubles peak memory
- **Difficulty:** medium
- **Location:** src/train_data/bm25.rs:55
- **Description:** `Bm25Index::build(docs: &[(String, String)])` takes a borrowed slice but immediately clones the entire corpus at line 55 with `docs: docs.to_vec()`. Each entry is `(String, String)` -- a content hash (~64 chars) + full function body (avg ~500 chars). For a large codebase with 20K callable functions, the corpus is ~12MB, and the clone doubles it to ~24MB. The cloned `docs` field is only used in `score()` and `select_negatives()` to map results back to content_hash and content. The `doc_terms` field already stores per-document term frequencies. The struct could take ownership instead of cloning.
- **Suggested fix:** Change the signature to `fn build(docs: Vec<(String, String)>) -> Self` (take ownership), remove the `.to_vec()`. Callers already have an owned `Vec` from `build_bm25_corpus`. Alternatively, store only content hashes and make `select_negatives` look up content from the original corpus via an index.

#### RM-3: `webhelp_to_markdown` merged string has no size limit -- 1000 pages can produce 100MB+ string
- **Difficulty:** easy
- **Location:** src/convert/webhelp.rs:117
- **Description:** The `merged` string at line 117 grows by concatenating HTML-to-Markdown output for up to `MAX_PAGES` (1000) pages. `html_file_to_markdown` enforces a 100MB per-file limit, but individual webhelp pages are small HTML pages. The real issue is the aggregate: 1000 pages averaging 100KB each produces a 100MB `String`. After conversion, the `cleaning::clean_markdown` pass at line 420 of `mod.rs` creates a second copy of similar size. Peak memory during webhelp conversion is approximately 2x the merged string size. There is no aggregate size limit -- only page count.
- **Suggested fix:** Add a `MAX_WEBHELP_BYTES` constant (e.g., 50MB). After each page concatenation, check `if merged.len() > MAX_WEBHELP_BYTES { tracing::warn!(...); break; }`. This bounds peak memory regardless of per-page size.

#### RM-4: Watch mode `last_indexed_mtime` pruning only triggers at >10K entries -- stale entries accumulate indefinitely below threshold
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:459-463
- **Description:** The `last_indexed_mtime: HashMap<PathBuf, SystemTime>` at line 64 records the mtime of every file that gets indexed. The pruning at lines 459-463 only fires when the map exceeds 10,000 entries: `if state.last_indexed_mtime.len() > 10_000 { .retain(...) }`. For a typical project with <10K source files, deleted or renamed files create orphan entries that never get cleaned. Over a multi-day watch session, these accumulate. Each entry is `PathBuf + SystemTime` (~100 bytes), so 9,999 orphans is ~1MB -- not dangerous, but the real cost is the `.retain()` check that calls `cfg.root.join(f).exists()` for every entry -- an O(N) filesystem stat when it finally does trigger.
- **Suggested fix:** Add periodic pruning on a time basis (e.g., every 1000 reindex cycles or every hour), not just a size threshold. Alternatively, lower the threshold to 2x the current file count at watch start: `if state.last_indexed_mtime.len() > initial_file_count * 2 { ... }`.

#### RM-5: Contrastive neighbors `per_row_neighbors` allocates N*(N-1) intermediate pairs before truncation
- **Difficulty:** medium
- **Location:** src/llm/summary.rs:252-271
- **Description:** The PERF-43 fix replaced BinaryHeap with `select_nth_unstable_by` for O(N) partial sort, but line 256 still allocates a fresh `Vec<(usize, f32)>` of N-1 candidates for every row: `(0..n).filter(|&j| j != i).map(|j| (j, row[j])).collect()`. At N=12,000 this is 12,000 * 11,999 * 12 bytes = ~1.6GB of intermediate allocations (not all live simultaneously, but each row's Vec is ~144KB and the allocator must find/return that block 12,000 times). The `sims` matrix (~550MB) is still alive at this point. Combined peak is ~700MB (sims + one row candidates Vec + per_row_neighbors accumulation). The `drop(matrix)` at line 247 frees ~49MB but `sims` stays until line 272.
- **Suggested fix:** Reuse a single `candidates` buffer across rows. Move `let mut candidates: Vec<(usize, f32)> = Vec::with_capacity(n - 1);` before the loop. Inside the loop: `candidates.clear(); candidates.extend(...)`. This reduces allocations from N to 1 and avoids 12,000 alloc/dealloc cycles. The extracted top-K can be copied into `per_row_neighbors` as a small Vec.

#### RM-6: Watch mode embedder `OnceCell` is never released on sustained errors -- backoff retries allocate new Embedder each attempt
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:112-139
- **Description:** `try_init_embedder` at line 126 calls `Embedder::new(model_config.clone())` on each retry attempt. `Embedder::new` allocates a `Mutex<LruCache>`, `OnceLock`, and `ModelConfig` clone (~5KB total). If initialization succeeds, `embedder.get_or_init(|| e)` stores it in the `OnceCell`. But if it fails, the partially-constructed `Embedder` (minus the ONNX session which failed) is dropped. With exponential backoff capped at 300s and a broken model, this is one 5KB allocation every 300s -- negligible individually. However, the `ModelConfig::clone()` at line 126 is the real cost: it clones the repo URL, ONNX paths, and prefix strings on every retry. With repeated failures during extended watch sessions, this is a minor but unnecessary allocation pattern.
- **Suggested fix:** Clone `model_config` once at watch startup (already done at line 255). The `try_init_embedder` function already receives a `&ModelConfig` reference, so `Embedder::new` should take `&ModelConfig` or the clone should happen once. Since `Embedder::new` takes `ModelConfig` by value, the clone is unavoidable without a signature change. Low priority -- the allocation is small and bounded by the backoff cap.

## Security

#### SEC-1: `cmd_index` creates `.cqs` directory without restrictive permissions
- **Difficulty:** easy
- **Location:** src/cli/commands/index.rs:70-73
- **Description:** `cmd_index` creates the `.cqs` directory via `create_dir_all` at line 71 but never sets permissions to 0o700. In contrast, `cmd_init` (src/cli/commands/init.rs:24-33) correctly creates the directory and immediately sets `0o700`. If a user runs `cqs index` on a fresh project (bypassing `cqs init`), the `.cqs` directory inherits the process umask -- typically 0o755, making the index database world-readable. The database contains source code content, function names, and embeddings. Watch mode also has this gap: `cmd_watch` calls `resolve_index_dir` which may create the directory without permissions.
- **Suggested fix:** Extract the `create_dir_all` + `set_permissions(0o700)` pattern from `cmd_init` into a shared `ensure_cqs_dir(path)` helper. Call it from `cmd_index`, `cmd_watch`, and anywhere else that creates the `.cqs` directory. Alternatively, add the `#[cfg(unix)] set_permissions` block after line 73 in index.rs.

#### SEC-2: Telemetry file created without restrictive permissions
- **Difficulty:** easy
- **Location:** src/cli/telemetry.rs:41
- **Description:** When `CQS_TELEMETRY=1`, `log_command` opens `telemetry.jsonl` with `OpenOptions::new().create(true).append(true).open(&path)` -- no mode restriction. On Unix this creates the file with default umask (typically 0o644 = world-readable). The telemetry file contains user queries, which may include function names, search terms, and code patterns -- potentially sensitive information about what a developer is working on. Every other file-creation path in cqs sets 0o600 (config, notes, lock files, DB, HNSW files).
- **Suggested fix:** On Unix, add `.mode(0o600)` via `OpenOptionsExt`: `use std::os::unix::fs::OpenOptionsExt; OpenOptions::new().create(true).append(true).mode(0o600).open(&path)`. Match the pattern used in `src/cli/files.rs:68`.

#### SEC-3: `ensure_model` does not verify joined paths stay inside `CQS_ONNX_DIR`
- **Difficulty:** medium
- **Location:** src/embedder/mod.rs:700-706
- **Description:** When `CQS_ONNX_DIR` is set, `ensure_model` joins it with `config.onnx_path` (line 702) and `config.tokenizer_path` (line 703) without verifying the resulting paths resolve inside the directory. For preset models this is safe (hardcoded relative paths like `"onnx/model.onnx"`). For custom models, SEC-20 validation in `models.rs:183` blocks `..` and absolute paths. However, SEC-20 only checks the string for literal `..` -- it does not canonicalize the joined result. On case-insensitive filesystems or with symlinks inside `CQS_ONNX_DIR`, the joined path could resolve outside the directory. More concretely: if an attacker can place a symlink inside the `CQS_ONNX_DIR` (e.g., `CQS_ONNX_DIR/onnx` -> `/tmp/evil/`), the model loaded would be from outside the intended directory. This is defense-in-depth -- the user controls `CQS_ONNX_DIR` anyway -- but other path-accepting code (read, convert, reference) all canonicalize and verify containment.
- **Suggested fix:** After joining, canonicalize the result and verify `canonical_model.starts_with(&dir)`. Pattern: `let model_path = dunce::canonicalize(dir.join(&config.onnx_path)).map_err(|e| EmbedderError::HfHub(format!("Model path: {}", e)))?; if !model_path.starts_with(&dir) { return Err(EmbedderError::HfHub("Model path escapes CQS_ONNX_DIR".into())); }`.

#### SEC-4: `convert_directory` output directory not validated against source containment
- **Difficulty:** medium
- **Location:** src/convert/mod.rs:251, src/cli/commands/convert.rs:22-34
- **Description:** `cqs convert <dir>` accepts an `--output` directory from CLI arguments. The `finalize_output` function at mod.rs:262 checks that the output path doesn't overwrite the source (self-overwrite guard), but there is no validation that the output directory is a reasonable location. A user could inadvertently run `cqs convert /path/to/docs --output /etc` and cqs would attempt to write Markdown files into `/etc`. While the user controls the CLI args and this isn't an injection vector, the `cqs read` command validates paths stay within the project root for defense-in-depth. The convert command should at minimum warn when writing outside the source tree, and the output directory should be canonicalized with `dunce::canonicalize` to normalize symlinks (currently it uses raw `PathBuf::from(dir)` at convert.rs:23).
- **Suggested fix:** Canonicalize `output_dir` in `cmd_convert`. Optionally add a warning (not a hard error) when the output directory is outside the source path's parent. The overwrite guard already canonicalizes both sides for comparison, so the fix is consistent.

#### SEC-5: `expand_query_for_fts` passes original token through without re-sanitization
- **Difficulty:** easy
- **Location:** src/search/synonyms.rs:68
- **Description:** The doc comment says "Input must already be FTS-sanitized" and the call chain (`sanitize_fts_query` -> `expand_query_for_fts`) ensures this in practice. However, `expand_query_for_fts` itself does not verify or enforce this contract -- it trusts the caller. At line 68, `format!("({}", token)` embeds the token directly into an OR group with parentheses. If a future caller passes unsanitized input, this becomes an FTS5 injection vector. The static synonyms are safe (known-good alpha strings), but the original token passes through raw. This is a defense-in-depth concern, not an active vulnerability.
- **Suggested fix:** Add a `debug_assert!` at function entry verifying the input contains no FTS5 special chars: `debug_assert!(sanitized_query.chars().all(|c| !matches!(c, '"' | '*' | '(' | ')' | '+' | '-' | '^' | ':' | '{' | '}')), "Input to expand_query_for_fts must be pre-sanitized");`. This catches misuse during development without runtime cost. Alternatively, add a comment noting the safety invariant (the existing doc comment is good but a `// SAFETY:` inline comment at line 68 would be more visible).

#### SEC-6: `search_by_name` constructs FTS query via `format!` with double-quote embedding
- **Difficulty:** easy
- **Location:** src/store/search.rs:82
- **Description:** `search_by_name` at line 82 builds an FTS5 query: `format!("name:\"{}\" OR name:\"{}\"*", normalized, normalized)`. The `normalized` value comes from `sanitize_fts_query` which strips double quotes, and there is both a `debug_assert!` (line 76) and a runtime check (line 79-81) that returns empty results if quotes are present. This is thorough. However, the defense relies entirely on `sanitize_fts_query` correctly stripping quotes -- if that function were ever modified to use an allowlist instead of a denylist, and quotes were accidentally allowed, this `format!` would become an injection point. The same pattern is repeated in `chunks/query.rs:389` for batch name search. Defense-in-depth would use parameterized column filtering instead of string interpolation.
- **Suggested fix:** Low priority. The triple-layer defense (sanitize + debug_assert + runtime check) is already solid. For maximum safety, consider using FTS5's `{column}:({terms})` syntax with bound parameters if SQLite supports it, or document the invariant with a `// SAFETY: sanitize_fts_query strips all double quotes; the debug_assert + runtime guard above are belt-and-suspenders` comment.

## Test Coverage

#### TC-1: reranker.rs — sigmoid NaN passthrough untested, only 6 unit tests for 388 lines
- **Difficulty:** easy
- **Location:** src/reranker.rs:216-218, src/reranker.rs:340-342
- **Description:** `sigmoid(f32::NAN)` returns `NaN`, and line 216 (`result.score = sigmoid(logit)`) assigns that directly to `result.score` without a finiteness check. The downstream `total_cmp` sort handles NaN ordering, but NaN scores leak to callers. The reranker has only 6 unit tests (sigmoid basic cases + constructor + empty results) for 388 lines of code covering model download, tokenization, inference, and scoring. No tests cover: (a) NaN logit from inference output, (b) `rerank` with a single result (early return at line 124 means it's never scored), (c) `rerank_with_passages` length mismatch (line 127-131 uses `assert_eq!` which panics rather than returning an error), (d) `clear_session` followed by `rerank` (lazy reinit path). Prior triage items TC-41 through TC-49 cover other modules; this is a new finding specific to the reranker.
- **Suggested fix:** Add a `sigmoid(f32::NAN)` test asserting the result is finite or handled. Add a finiteness guard at line 216: `let logit = data[i * stride]; let s = sigmoid(logit); result.score = if s.is_finite() { s } else { 0.0 };`. Add unit tests for single-result input and passages-length-mismatch (change `assert_eq!` to return `Err`).

#### TC-2: structured_text.rs — no tests for method_definition, action_definition, or type extraction
- **Difficulty:** easy
- **Location:** src/language/structured_text.rs:17-23, src/language/structured_text.rs:33-66
- **Description:** The ST language definition declares tree-sitter queries for `method_definition` (line 17), `action_definition` (line 19), and a full `TYPE_QUERY` for extracting type references from VAR declarations, array types, struct fields, EXTENDS clauses, and return types (lines 33-66). The existing 5 tests cover function_block, function, program, type_definition, and call graph. No test verifies that methods or actions parse correctly, and no test verifies type reference extraction. Since ST is the newest language (post-v1.13.0), these queries have never been exercised by any test. If the tree-sitter grammar has different node names than assumed, the queries silently return zero results.
- **Suggested fix:** Add tests: (1) method inside a FUNCTION_BLOCK (`METHOD DoCalc : REAL ... END_METHOD`), (2) action inside a FUNCTION_BLOCK (`ACTION Reset ... END_ACTION`), (3) type extraction from a FUNCTION_BLOCK with typed VAR_INPUT and EXTENDS clause.

#### TC-3: rerank_with_passages assert_eq! on length mismatch panics instead of returning Err
- **Difficulty:** easy
- **Location:** src/reranker.rs:127-131
- **Description:** `rerank_with_passages` uses `assert_eq!(results.len(), passages.len(), ...)` at line 127. This panics on mismatch instead of returning `Err(RerankerError::Inference(...))`. In production, this is called from `rerank()` (line 102) which constructs passages from results so lengths always match, but the function is `pub` and exposed for external use (e.g., NL-description reranking). A mismatched-length caller gets a panic instead of a recoverable error. No test covers the mismatch case. The prior triage has no entry for this — TC-46 covers batch.rs, not reranker.
- **Suggested fix:** Replace `assert_eq!` with: `if results.len() != passages.len() { return Err(RerankerError::Inference(format!("passages length {} != results length {}", passages.len(), results.len()))); }`. Add a test: `rerank_with_passages("q", &mut results, &["too", "few"], 10)` asserting `Err`.

#### TC-4: score_candidate — no NaN embedding adversarial test
- **Difficulty:** easy
- **Location:** src/search/scoring/candidate.rs:225-262, src/search/scoring/candidate.rs:789-815
- **Description:** `score_candidate` has a zero-embedding test (`score_candidate_zero_embedding`, line 789) but no NaN-embedding test. While `cosine_similarity` returns `None` for NaN inputs (guarding the path), this invariant is tested only in `math.rs`, not at the `score_candidate` level. If `cosine_similarity`'s NaN handling were ever relaxed (e.g., switching SIMD backends), `score_candidate` would propagate NaN scores into `BoundedScoreHeap`. The heap has a NaN test (`test_bounded_heap_ignores_non_finite`, line 298) but `score_candidate` itself lacks one. The zero-embedding test is a weaker variant — zero vectors produce `Some(0.0)` or `None`, never NaN.
- **Suggested fix:** Add `score_candidate_nan_embedding` test alongside `score_candidate_zero_embedding`: create a query with `vec![f32::NAN; EMBEDDING_DIM]`, call `score_candidate`, assert the result is `None`. This documents the safety contract at the scoring layer rather than relying on transitive trust in `cosine_similarity`.

#### TC-5: suggest_placement_with_options — zero direct tests, only integration coverage
- **Difficulty:** medium
- **Location:** src/where_to_add.rs (suggest_placement_with_options)
- **Description:** `suggest_placement_with_options` is the core placement logic accepting `PlacementOptions` (including pre-computed `query_embedding` for embedding reuse). It has zero direct unit tests. All coverage comes through integration paths: `task_with_resources` (which catches errors and returns empty vec), `cmd_where` (CLI), and `dispatch_where` (batch). The `PlacementOptions` struct includes `query_embedding: Option<Embedding>` for the optimization added in task.rs, but no test verifies that providing a pre-computed embedding skips the redundant ONNX inference. The outer `suggest_placement` wrapper (which calls `suggest_placement_with_options` with default options) has an integration test in `tests/where_test.rs`, but that doesn't exercise the `query_embedding` path.
- **Suggested fix:** Add a unit test that passes `PlacementOptions { query_embedding: Some(mock_embedding) }` and verifies results are returned without requiring an embedder session (proving the pre-computed path is used). Add an edge case test with empty store.

## Data Safety

#### DS-38: All SQLite write transactions use DEFERRED mode -- concurrent `cqs index` causes SQLITE_BUSY
- **Difficulty:** medium
- **Location:** src/store/chunks/crud.rs:52, src/store/migrations.rs:43, and 16 other `pool.begin()` sites
- **Description:** Every `pool.begin().await?` in the store starts a `BEGIN DEFERRED` transaction. In SQLite WAL mode, DEFERRED transactions that write only acquire the write lock when the first write statement executes. If two `cqs index` processes run concurrently, one will hit SQLITE_BUSY when it tries to write inside its deferred transaction. The 5-second busy timeout helps, but under heavy concurrent writes (batch upserts of thousands of chunks), a 5-second wait may not be enough, and there is no retry logic -- a single busy timeout causes the entire batch to fail with an opaque sqlx error. There is also no process-level lock to prevent concurrent `cqs index` runs. `cqs watch` has no guard against a user running `cqs index` simultaneously.
- **Suggested fix:** Add a process-level advisory file lock (e.g., `.cqs/index.lock`) in `cmd_index` and watch mode, acquired before the first write. Use `BEGIN IMMEDIATE` for write transactions so lock contention surfaces at `BEGIN` rather than mid-transaction (cleaner error). Document in `--help` that concurrent writes are not supported.

#### DS-39: `bytes_to_embedding` silently returns `None` for dimension-mismatched embeddings -- search returns empty results with no error
- **Difficulty:** medium
- **Location:** src/store/helpers.rs:900-911, src/store/chunks/query.rs:266, src/store/chunks/embeddings.rs:48
- **Description:** When the stored `dimensions` metadata changes (e.g., migration v14->v15 changed 769->768, or switching from E5-base 768 to BGE-large 1024), `bytes_to_embedding` silently returns `None` for every existing embedding because `bytes.len() != expected_dim * 4`. Callers skip `None` results, so `get_chunk_with_embedding` returns `None` with only a trace-level log, `get_embeddings_by_hashes` silently omits mismatched entries, and brute-force search (`search_by_candidate_ids_async`) filters them out. The net effect: after a model change without `--force` rebuild, search returns zero results with no user-visible warning. The v14->v15 migration correctly sets `hnsw_dirty=1` which causes HNSW fallback-to-brute, but brute-force itself also silently returns nothing because all stored embeddings are the wrong dimension.
- **Suggested fix:** On `Store::open`, compare `self.dim` against a sampling of actual embedding byte lengths from `chunks` (e.g., `SELECT LENGTH(embedding) FROM chunks LIMIT 1`). If `actual_bytes != dim * 4`, emit a warning: "Stored embeddings are {actual_dim}-dim, expected {dim}-dim. Run 'cqs index --force' to rebuild." This catches both migration gaps and model-switch scenarios without requiring a full table scan.

#### DS-40: Migration v15->v16 uses DROP TABLE inside transaction -- idempotency failure on retry after partial rollback
- **Difficulty:** easy
- **Location:** src/store/migrations.rs:205-236
- **Description:** `migrate_v15_to_v16` creates `llm_summaries_v2`, copies data from `llm_summaries`, drops `llm_summaries`, then renames `llm_summaries_v2` to `llm_summaries`. The migration runs inside a single transaction so crash safety is fine. However, the `CREATE TABLE IF NOT EXISTS llm_summaries_v2` means that if a previous migration attempt failed *after* creating `llm_summaries_v2` but *before* committing, a stale `llm_summaries_v2` table may exist from the rolled-back transaction on the next attempt. SQLite DDL rollback is documented to work, so this should be safe in practice. But a more subtle issue: if the migration is re-run against a DB where v16 was partially applied outside a transaction (e.g., manual SQL), the `DROP TABLE llm_summaries` will succeed but `llm_summaries_v2` might already be named `llm_summaries`, causing the `RENAME` to fail. The migration does not check the current schema state defensively.
- **Suggested fix:** Add a guard at the top of `migrate_v15_to_v16`: check if `llm_summaries` already has the `purpose` column (via `PRAGMA table_info`). If so, skip the migration. This makes the migration truly idempotent regardless of how the DB reached its current state.

#### DS-41: `set_hnsw_dirty(true)` failure in watch mode skips reindex but leaves files in mtime cache -- permanently skipped
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:447-450
- **Description:** When `set_hnsw_dirty(true)` fails (e.g., disk full, SQLite locked), watch mode returns early from the reindex handler. However, the file mtime collection (`pre_mtimes`) was already gathered before this check. The `pre_mtimes` map is *not* written to `state.last_indexed_mtime` (that happens later on success), so the files will be re-detected on the next watch tick. This is correct. But the related pattern in `cmd_index` at index.rs:147 only logs a warning and continues indexing despite the dirty flag not being set. If the process then crashes between SQLite commit and HNSW save, the missing dirty flag means the next load will trust the stale HNSW index.
- **Suggested fix:** In `cmd_index` (index.rs:147), promote the `set_hnsw_dirty` failure from a warning to an error that aborts indexing. The dirty flag is a safety invariant -- if it can't be set, the crash-safety guarantee is voided. Watch mode already handles this correctly.

#### DS-42: `prune_all` identifies missing files outside transaction -- TOCTOU with concurrent file creation
- **Difficulty:** easy
- **Location:** src/store/chunks/staleness.rs:122-149
- **Description:** `prune_all` has two phases: Phase 1 queries all distinct origins from `chunks` and filters against `existing_files` in Rust (outside any transaction). Phase 2 opens a transaction and deletes chunks for the `missing` list. Between Phase 1 and Phase 2, a concurrent `cqs index` or `cqs watch` could insert chunks for a file that was in the `missing` list. The Phase 2 `DELETE FROM chunks WHERE origin IN (...)` would then delete the freshly-inserted chunks. This is a TOCTOU race. In practice the window is small (milliseconds), and `prune_all` is called at the end of `cqs index` when watch mode shouldn't be running. But if both run concurrently (no process lock prevents this), newly-indexed chunks could be silently deleted.
- **Suggested fix:** Move Phase 1 inside the transaction (use a CTE or subquery): `DELETE FROM chunks WHERE origin IN (SELECT DISTINCT origin FROM chunks WHERE source_type = 'file') AND origin NOT IN (...)` with the `existing_files` set passed as bound parameters. This makes the identification and deletion atomic. Alternatively, the process-level lock from DS-38 would prevent concurrent writers entirely.

#### DS-43: Batch mode `check_index_staleness` re-opens Store on mtime change but does not reload HNSW dimension
- **Difficulty:** medium
- **Location:** src/cli/batch/mod.rs:130-155
- **Description:** When `check_index_staleness` detects that `index.db` mtime changed, it invalidates mutable caches (clearing the HNSW `RefCell`) and re-opens the Store. The new Store reads the correct `dim` from metadata. However, when `vector_index()` is subsequently called, it calls `build_vector_index` which passes `store.dim()` to `HnswIndex::try_load_with_ef`. This is correct for the fresh Store. But `BatchContext.model_config` is a field set at construction time and never updated on invalidation. If the model changed between sessions (e.g., user edited `.cqs/config.toml` to switch from E5-base to BGE-large), `model_config.dim` would be stale. The embedder (cached in `OnceLock`) would still produce embeddings at the old dimension, while the Store expects the new dimension. Queries would produce wrong-dimension embeddings and get zero search results.
- **Suggested fix:** On `check_index_staleness` invalidation, also re-read the config file and update `model_config`. Or check if `store.dim()` differs from `model_config.dim` after re-opening, and if so, log a warning and clear the embedder `OnceLock` (though `OnceLock` cannot be cleared -- would need to switch to `RefCell<Option<Embedder>>`).
