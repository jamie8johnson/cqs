## Test Coverage (happy path) — v1.38 batch 2

Audit context: branch `main` at `d9d5ce6f` (post-v1.38.0). v1.36.2 triage closed all TC-HAP-V1.36-* via #1488/#1489/#1490/#1497 + cli_train_data_test/cli_project_search_test. Recent #1505/#1506/#1507 shipped parse-only / unit-only tests for new feature surface; #1511 still open. New gaps cluster around (a) parse-only tests for new flag surface, (b) `enrichment_pass` carve-out added by #1497, (c) cross-project handlers, (d) `cmd_dead`/`cmd_explain`/`cmd_install` end-to-end.

---

#### TC-HAP-V1.38-1: `cqs project search` filter knobs (#1507) only have a parse test, no behavioral assertions
- **Difficulty:** medium
- **Location:** `src/cli/commands/infra/project.rs` (`cmd_project::Search` arm) — and `tests/cli_project_search_test.rs:84` only exercises the no-filter path.
- **Description:** PR #1507 added `--lang`, `--include-type`, `--exclude-type`, `--path`, `--name-boost`, `--rrf`, `--include-docs` to `cqs project search`. The only test is `src/cli/mod.rs:463 test_cmd_project_search_full_flag_parity` which checks clap parsing — never runs the search. A regression in the SearchFilter wire-through (e.g., `--lang rust` silently ignored, or `--include-type function` mistakenly skipped) would slip through CI. Agent calling `cqs project search "license activation" -l rust` would silently get unfiltered results and not notice.
- **Suggested fix:** In `tests/cli_project_search_test.rs`, add `test_project_search_lang_filter_excludes_other_languages`. Set up two projects: one with rust + python content (matching name `foo`), one with only rust content. Run `cqs project search foo -l rust -t 0.0 -n 50`, assert all results have file extensions matching rust. Then run `cqs project search foo --include-type function -t 0.0 -n 50`, assert no `chunk_type == "test"` results.

#### TC-HAP-V1.38-2: `cqs ref reindex --llm-summaries` flag wire-through has parse-only test, never runs the LLM/HyDE pass
- **Difficulty:** medium
- **Location:** `src/cli/commands/infra/reference.rs:599 cmd_ref_update` (LLM/HyDE/doc passes at lines 757-870).
- **Description:** PR #1506 added 6 LLM/HyDE flags to `cqs ref reindex`. The two new tests (`src/cli/mod.rs:461 test_cmd_ref_reindex_llm_flags_parse`, `:505 test_cmd_ref_reindex_apply_flag_rejected`) are clap-only. The actual passes (`llm_summary_pass(&store, ..., Some(ref_dir))`, `doc_comment_pass`, `hyde_query_pass`, `enrichment_pass`) running against the **ref** store with `Some(ref_dir)` are unverified end-to-end. Pre-#1506 path was byte-identical without the LLM passes; a wiring bug in the new path (e.g., `Some(ref_dir)` accidentally `None` so doc patches land in the wrong tree) wouldn't surface. The PR's own "Smoke post-merge" checkbox is still unchecked.
- **Suggested fix:** Extend `tests/cli_ref_test.rs` with `#[ignore]`-gated `test_ref_reindex_llm_summaries_writes_to_ref_store_not_project`. Set up one ref + one project sharing source. Run `cqs ref reindex <name> --llm-summaries`. Assert summaries land in `ref_dir/index.db` (`SELECT count(*) FROM llm_summaries`) and **not** in the project store. Mock LLM by short-circuiting `cqs::llm::llm_summary_pass` via a feature flag or by stubbing the cache so a deterministic summary is written.

#### TC-HAP-V1.38-3: `enrichment_pass` itself has zero direct tests despite PR #1497 adding the `needs_embedding=1` carve-out
- **Difficulty:** medium
- **Location:** `src/cli/enrichment.rs:23 enrichment_pass` (the carve-out at lines 65-84 + 195-241).
- **Description:** Only the `compute_enrichment_hash_with_summary` helper is covered (`src/cli/enrichment.rs:460-545`). The pass itself — including the new branch that forces `needs_embedding=1` chunks past the early-skip with an empty `CallContext` for ambiguous names — is exercised only through the production `cqs index --llm-summaries` path. A regression that drops the carve-out (so `--llm-summaries` ships zero-vec sentinels forever) would only show up in a manual smoke test. The crud.rs round-trip (`unembedded_chunks_invisible_from_name_search`) fakes the enrichment by calling `update_embeddings_with_hashes_batch` directly, bypassing `enrichment_pass`.
- **Suggested fix:** Add `enrichment_pass_promotes_needs_embedding_chunks` in a new `src/cli/enrichment.rs#tests` block (or a `tests/enrichment_test.rs` integration test). Seed a store with: one chunk at `needs_embedding=1` (no callers, no callees, no summary), one normal chunk. Run `enrichment_pass(&store, &embedder, &model_config, true)`. Assert: (1) the flagged chunk's `needs_embedding` flips to 0, (2) its embedding bytes are non-zero post-pass, (3) `search_by_name(name, 10)` now returns it. `#[ignore]`-gate for the embedder cold-load (matches `prepare_for_embedding` pattern in #1489).

#### TC-HAP-V1.38-4: `cqs index --model X` drift detection has unit test for the helper, no end-to-end CLI test
- **Difficulty:** easy
- **Location:** `src/cli/commands/index/build.rs:357` (call site) + `:1501 check_index_model_drift` (helper).
- **Description:** PR #1505 ships 4 unit tests on the helper (`check_index_model_drift_*`) but the integration path — that `cmd_index` actually invokes the check at the right time and the error reaches the user — is unverified. `tests/cli_doctor_fix_test.rs` shells out to `cqs index --force` already, so the subprocess infrastructure exists. PR's own "Smoke post-merge" is unchecked. Pre-1505 footgun was silent dim-mismatch corruption; a regression that bypasses the check (e.g., someone moves the call after `store.init`) would re-introduce silent corruption with no test to catch it.
- **Suggested fix:** Add `tests/cli_index_model_drift_test.rs` (slow-tests gated). Index a tiny project with default model. Re-run `cqs index --model embeddinggemma-300m` (no `--force`). Assert exit code != 0, stderr contains "was built for model" and "--force". Then run with `--force --model embeddinggemma-300m`, assert exit 0 and `cqs status --json` reports the new model.

#### TC-HAP-V1.38-5: `cmd_dead` (CLI handler) is untested end-to-end — only `find_dead_code` and `DeadOutput` JSON shape covered
- **Difficulty:** easy
- **Location:** `src/cli/commands/review/dead.rs:76 cmd_dead`. Existing tests (lines 180, 195) only assert serialization shape on hand-built structs.
- **Description:** `tests/dead_code_test.rs` exercises the library `find_dead_code` against a TestStore but never invokes `cmd_dead`. The CLI wrapper translates between the library output, two CLI flags (`--include-pub`, `--limit`), and the JSON envelope. A bug like swapping `dead` and `possibly_dead_pub` in the envelope, or `--include-pub` being silently ignored, has no regression guard. Agents query `cqs dead --json` and consume `data.dead[]`; a wrong field name there breaks every consumer.
- **Suggested fix:** Add `test_dead_cli_emits_envelope_with_expected_fields` to `tests/cli_surface_test.rs` (already has cqs subprocess infra). Index a tiny project with one definitely-dead function, run `cqs --json dead`. Assert envelope has `data.dead[]`, `data.count >= 1`, the dead function's name appears in `data.dead[*].name`. Re-run with `--include-pub`, assert `possibly_dead_pub` array surfaces a pub fn with no callers.

#### TC-HAP-V1.38-6: `cmd_explain` (CLI handler) has no end-to-end test — `tests/graph_test.rs:explain_*` reimplement the BFS in-process
- **Difficulty:** easy
- **Location:** `src/cli/commands/graph/explain.rs:303 cmd_explain`. Inline tests (lines 458-587) only check `ExplainOutput` JSON field names.
- **Description:** Same gap reported as TC-HAP-V1.33-5 — still open after v1.38. `tests/graph_test.rs:explain_process_reports_callers_and_callees` builds an InProcessFixture and asserts on direct store queries, never goes through `cmd_explain`. Agents calling `cqs explain <fn> --json` rely on the envelope shape including `callers[]`, `callees[]`, `tests[]`, `notes[]`, `hints[]`. A regression in the CLI handler's envelope assembly (e.g., `notes` accidentally dropped after the AuditMode load) would not be caught.
- **Suggested fix:** Add `tests/cli_explain_test.rs` (slow-tests gated, mirror `cli_brief_test.rs` setup). Index a project with main → process → validate. Run `cqs --json explain process`. Assert `data.callers[*].name == "main"`, `data.callees[*].name == "validate"`, `data.target == "process"`. Add a note mentioning `process`, re-run, assert `data.notes[]` non-empty.

#### TC-HAP-V1.38-7: `cmd_install` and `cmd_status` for git hooks have no direct test — only `write_hook_script`
- **Difficulty:** medium
- **Location:** `src/cli/commands/infra/hook.rs:186 cmd_install` and `:474 cmd_status`. v1.33.0-pre flagged this as TC-HAP-1.30.1-2/3; existing tests at lines 712-720 explicitly note "drives the lower-level `write_hook_script` directly because `cmd_install` would resolve the project root from the workspace, not the temp dir."
- **Description:** Both wrappers handle CWD discovery, JSON-vs-text branching, marker version detection, and dispatch to `write_hook_script`. None of that integration is tested. Status' "behaviour matrix promises 6 outcomes, zero are pinned." Workaround used in tests sidesteps the very thing that breaks (CWD discovery / version detection).
- **Suggested fix:** Add `tests/cli_hook_test.rs` (no slow-tests gate — no embedder needed). For `cmd_install`: create a tempdir with `.git/hooks/`, `cd` into it via `Command::current_dir`, run `cqs hook install`. Assert each of the 5 hook files exists and contains `HOOK_MARKER_PREFIX`. For `cmd_status`: install hooks, then add a foreign hook, then a v0-marker hook; run `cqs --json hook status`. Assert envelope reports correct (`installed`, `foreign`, `outdated`) classification per hook name.

#### TC-HAP-V1.38-8: `cmd_trace --cross-project` (cross_project arm of `cmd_trace`) has zero tests
- **Difficulty:** medium
- **Location:** `src/cli/commands/graph/trace.rs:114-178` (the `if cross_project { ... }` arm). Existing tests (lines 446-560) only cover BFS helpers and Mermaid escaping.
- **Description:** `cqs trace --cross-project src tgt` invokes `cqs::cross_project::trace_cross` against multiple registered projects. The arm has its own JSON envelope construction (`CrossProjectTraceResult`), Mermaid output with project tags (`format!("{} [{}]", ...)`) and text output with project hints (line 174-177). None covered. A regression that drops the project tag or wrong-typed `depth` (e.g., `len()` vs `len() - 1`) goes uncaught. Cross-project search itself has integration tests via `cli_project_search_test.rs`; cross-project trace has none.
- **Suggested fix:** Add `tests/cli_cross_project_trace_test.rs` (slow-tests gated, follow `cli_project_search_test.rs:setup_project_with` pattern). Two projects, each with `main → helper`. Register both. Run `cqs --json trace --cross-project main helper`. Assert envelope has `data.found == true`, `data.path[*].project` set, `data.depth == 1`. Re-run with `--format mermaid` and assert stdout contains `graph TD` + a `[<project>]` tag.

#### TC-HAP-V1.38-9: `Reranker::test_reranker_new` is a weak assertion — only checks construction succeeds, never asserts model loads or a known query/passage pair scores plausibly
- **Difficulty:** easy
- **Location:** `src/reranker.rs:1250-1254`. Asserts `OnnxReranker::new().is_ok()` and stops.
- **Description:** "No model download yet — lazy" comment confirms the test only proves the struct constructor runs. The whole point of the reranker is producing scores; the only behavioral test is `test_rerank_empty_results` which asserts `result.is_ok() && results.is_empty()` — also a tautology. A regression where rerank silently returns input unchanged (lost scoring) would pass. The `#[ignore]`-gated test below at line ~1265 is the real coverage but isn't run in CI.
- **Suggested fix:** Drop `test_reranker_new` (covered by the `#[ignore]` test). For `test_rerank_empty_results`, after the empty assertion add a second case with two known-distinct passages ("apple fruit pie" vs "deep learning model") and query "machine learning". Construct `SearchResult` stubs with stub embeddings. Even without a real model load, assert `rerank` returned the input list unchanged when the lazy model path no-ops on empty/short input. Better yet, mark the meaningful coverage as `#[cfg(feature = "reranker-tests")]` and run it in CI on the GPU runner.

#### TC-HAP-V1.38-10: `test_save_writes_file_with_0o600_perms` is `#[cfg(all(unix, target_os = "linux"))]` — macOS coverage is absent for a Unix-shared codepath
- **Difficulty:** easy
- **Location:** `src/project.rs:493-495` (gate line 493).
- **Description:** `ProjectRegistry::save` uses `std::os::unix::fs::PermissionsExt::set_mode(0o600)` which works identically on macOS. The test gate excludes macOS for no documented reason — likely copy-paste from a CI matrix concern. macOS is a first-class target (release artifact ships for `aarch64-apple-darwin`); a regression that drops the chmod (e.g., a refactor to `tempfile::persist_with_mode` that silently ignores umask on macOS only) would slip through. Same risk for `slot.toml` write paths.
- **Suggested fix:** Change the gate from `#[cfg(all(unix, target_os = "linux"))]` to `#[cfg(unix)]` so macOS runners exercise it. While there, audit other `target_os = "linux"`-only Unix permission tests — `grep -rn 'target_os = "linux"' src/ tests/` for Unix-permission tests that should also run on macOS. Tracked separately if more sites surface.

---

### Summary

10 findings. Concentration: parse-only tests landed for the v1.38 cohort (#1505/#1506/#1507), but behavioral assertions on the new flag surface are missing. `enrichment_pass` was extended by #1497 without growing direct test coverage. Three older TC-HAP gaps (cmd_explain, cmd_install, cmd_dead) remain open after v1.38. cmd_trace cross-project arm and reranker's smoke tests are weak. One macOS-side cross-platform gap on a Unix-shared file-permission test.

Recommended ordering: 1, 2, 3 (highest impact — gate the v1.38 cohort's actual behavior), then 5/6/7 (close the v1.30.1/v1.33 backlog), then 4/8 (defense for paths agents hit daily), then 9/10 (assertion-quality cleanups).
