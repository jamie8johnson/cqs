# Audit Findings — v1.29.0

Version: 1.29.0 (post-release, commit 04ee9516)
Date: 2026-04-23
Categories: 16 across 2 batches of 8 parallel opus auditors.

---


## Code Quality

#### `BatchProvider::submit_batch` (4-arg) and its 3 impls are dead code — zero production or test callers
- **Difficulty:** easy
- **Location:** `src/llm/provider.rs:27-33` (trait method), `src/llm/batch.rs:378-387` (LlmClient impl), `src/llm/provider.rs:100-108` (MockBatchProvider), `src/llm/provider.rs:164-172` (DefaultValidationProvider)
- **Description:** The `BatchProvider` trait declares `submit_batch(items, max_tokens, purpose, prompt_builder)` as a first-class method and three impls (including the production `LlmClient` one) faithfully implement it. Grepping `\.submit_batch\b(` across the crate returns zero call sites. All three real batch flows (summary, hyde, doc-comment) go through closures that call `submit_batch_prebuilt` / `submit_doc_batch` / `submit_hyde_batch` (see `src/llm/summary.rs:121`, `src/llm/hyde.rs:86`, `src/llm/doc_comments.rs:281`). The 4-arg trait method exists only as a shape for future callers that never came — it's carrying roughly 60 lines of impl code, a matching `submit_batch_inner` adapter in `LlmClient`, and the `prompt_builder: fn(&str, &str, &str) -> String` type parameter in the trait. Dead weight that blocks cleanup of the `BatchProvider` API and keeps the trait from being a narrow contract that matches actual usage.
- **Suggested fix:** Delete `BatchProvider::submit_batch` from the trait, delete the three impls (`src/llm/batch.rs:378-387`, `src/llm/provider.rs:100-108`, `src/llm/provider.rs:164-172`), and delete `LlmClient::submit_batch_inner`'s 4-arg wrapping logic if nothing else consumes it after the trait shrinks. Leaves `submit_batch_prebuilt` / `submit_doc_batch` / `submit_hyde_batch` as the real contract.

#### `cmd_scout` uses `build_scout_output` but `dispatch_scout` duplicates the logic inline — the "shared" helper isn't shared
- **Difficulty:** easy
- **Location:** `src/cli/commands/search/scout.rs:26-38` (the "shared" helper), `src/cli/batch/handlers/misc.rs:185-196` (the inline duplicate)
- **Description:** The docstring on `build_scout_output` explicitly says "shared between CLI and batch" and references CQ-V1.25-7 for the drift-risk rationale: "so the shape stays identical across the two dispatch paths." But only `cmd_scout` (src/cli/commands/search/scout.rs:68) calls it. `dispatch_scout` rolls its own: manually calls `serde_json::to_value(&result)`, then `inject_content_into_scout_json`, then `inject_token_info`. The current happy-path output is identical by coincidence — any change to `build_scout_output` (e.g. a new field, a new injection order, a span, error handling) will silently diverge between CLI and daemon socket, which is the exact regression the helper was introduced to prevent. `grep -rn "build_scout_output" src/` shows 0 batch callers; this is the same gap that bit `build_batched_with_dim` pre-v0.9.0 ("configurable models disaster" in CLAUDE.md).
- **Suggested fix:** In `src/cli/batch/handlers/misc.rs:185-196`, replace the inline sequence with a single call to `crate::cli::commands::search::scout::build_scout_output(&result, content_map.as_ref(), token_info)`. Either re-export or change the visibility so the batch handler can reach it (currently `pub(crate)`). Same pattern as `build_related_output` / `build_where_output` already use.

#### `cmd_similar` has a private `resolve_target` that silently diverges from `cqs::resolve_target` used by the batch path
- **Difficulty:** easy
- **Location:** `src/cli/commands/search/similar.rs:16-39` (CLI-only local helper), `src/search/mod.rs:47-94` (library function used by batch)
- **Description:** `cmd_similar` defines its own `resolve_target` local helper (line 16) that calls `store.search_by_name(func_name, 20)` then picks `results[0]` when no file filter matches. The library's `cqs::resolve_target` (which `dispatch_similar` at `src/cli/batch/handlers/info.rs:113` uses, and `cmd_neighbors` at `src/cli/commands/search/neighbors.rs:181` uses, and the helper wrapper at `src/cli/commands/resolve.rs:19-22` uses) differs in two load-bearing ways: (1) it prefers non-test chunks when names are ambiguous (lines 83-93) — the local helper always picks `results[0]`; (2) on file-filter miss it returns a structured `StoreError::NotFound` with a "Found in: foo.rs, bar.rs, baz.rs" hint (lines 68-81) — the local helper silently falls back to `results[0]`. Net effect: `cqs similar Foo` via CLI may return a test chunk or silently pick the wrong file, while `echo 'similar Foo' | cqs batch` returns the non-test chunk with a helpful error for typo'd file filters. The duplication also means any future fix to one won't reach the other.
- **Suggested fix:** Delete the local `resolve_target` fn in `src/cli/commands/search/similar.rs:16-39` and replace its one call site (line 59) with `cqs::resolve_target(store, name)?` (or `crate::cli::commands::resolve::resolve_target`, which wraps it with anyhow context). The local `parse_target` import on line 13 becomes unused — drop it.

#### Risk thresholds duplicated in `cmd_affected`: `overall_risk_label` (JSON) and inline text path use identical cutoffs independently
- **Difficulty:** easy
- **Location:** `src/cli/commands/review/affected.rs:92-100` (`overall_risk_label` for JSON), `src/cli/commands/review/affected.rs:143-149` (inline text-path ladder)
- **Description:** Both pieces of code compute the same risk banding — `>10 callers or >5 changed → high`, `>3 callers or >2 changed → medium`, else `low` — but from independent code paths. The JSON path emits string literals ("high" / "medium" / "low"); the text path emits `RiskLevel` enum variants then colors them via `risk_label`. Change one threshold and the two surfaces disagree silently. Separately, `empty_affected_json` at line 82-90 hardcodes `"overall_risk": "none"` — a fourth string that `overall_risk_label` never produces, so JSON consumers see "low" vs "none" distinction that only the empty-path emits. One bug waiting to happen every time someone tunes the cutoffs.
- **Suggested fix:** Extract a single `overall_risk(result: &DiffImpactResult) -> RiskLevel` (or `RiskLevel::from_diff_impact(...)` on the library enum) that both paths call. JSON path calls `.to_string()` or a dedicated `risk_level_json_label` and the text path wraps it in `risk_label(level)`. Decide whether "no changes" should be `RiskLevel::Low` or introduce a `RiskLevel::None` variant and fold `empty_affected_json` through the same function. Same change applies to any future `cmd_ci` / `cmd_review` that needs to emit a risk score.

#### "Empty impact diff" JSON object duplicated across 3 files, 4 sites — with a silent field-count drift
- **Difficulty:** easy
- **Location:** `src/cli/batch/handlers/graph.rs:402-407` and `:412-417` (inline in `dispatch_impact_diff`), `src/cli/commands/graph/impact_diff.rs:12-19` (`empty_impact_json`), `src/cli/commands/review/affected.rs:82-90` (`empty_affected_json`, + `overall_risk` field)
- **Description:** Four places all hand-roll the same `{ "changed_functions": [], "callers": [], "tests": [], "summary": { "changed_count": 0, "caller_count": 0, "test_count": 0 } }` object. Three are byte-identical; the fourth (`empty_affected_json`) adds `"overall_risk": "none"`. If `DiffImpactResult`'s serialized shape gains a new field (e.g. a `stale_count` counter, a `truncated` flag — note the P2 #32 surrounding work already added `truncated` to the real path), the empty path will diverge from the populated path on three commands (`cqs affected`, `cqs impact-diff`, `cqs batch impact-diff`). Agents and CI scripts parsing these will hit missing-field errors depending on whether the diff had hunks or not.
- **Suggested fix:** Add `DiffImpactResult::empty()` (or `diff_impact_empty_json()` next to `diff_impact_to_json`) in `src/impact/` and use it from all four sites. Make `cmd_affected` tack on `"overall_risk"` via the shared `overall_risk` helper (per previous finding). Then deleting fields from the populated JSON automatically removes them from the empty JSON too.

#### `cqs doctor` reports the compile-time `MODEL_NAME` constant as the index's "metadata" model — silently wrong after `cqs model swap`
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/doctor.rs:144-147, 155-156` (text + check-record)
- **Description:** `cmd_doctor` formats the model line as `"{} Model: {} (metadata: {})"` where the second placeholder is `cqs::embedder::model_repo()` (what the runtime resolves to) and the third is `cqs::store::MODEL_NAME` — a `pub const` that expands to `ModelConfig::DEFAULT_REPO` at compile time (`src/store/mod.rs:126`, `src/store/helpers/mod.rs:97`). Labeling it `metadata:` implies it was read from the store's metadata table, but it's actually identical to the hardcoded default on every invocation. If someone ran `cqs init --model v9-200k`, or `cqs model swap`, or pointed at an older index, doctor will confidently report that the index metadata matches BGE-large even when the store's `model_name` row says `sentence-transformers/mpnet-base` or `v9-200k`. `Store::stored_model_name()` (src/store/metadata.rs:152) is the correct source and exists. This is the same compile-time-constant-masquerading-as-live-state shape as the "configurable models disaster" that CLAUDE.md flags.
- **Suggested fix:** In `src/cli/commands/infra/doctor.rs` around line 137-158, open the store (read-only is fine — doctor already opens it for the index check) and call `store.stored_model_name()`. Display that with `.unwrap_or("unset")` as the `metadata:` value. If the stored name differs from `model_repo()`, promote the check-record from `ok` to a warning so doctor flags the mismatch instead of hiding it. Drop the `cqs::store::MODEL_NAME` reference; it's only meaningful inside `check_model_version_with` (itself `#[cfg(test)]`).

Wrote 6 findings to batch1-code-quality.md
---

## Documentation

#### CONTRIBUTING.md Architecture Overview says "Schema v20" but actual is v22
- **Difficulty:** easy
- **Location:** `CONTRIBUTING.md:193,207`
- **Description:** Two references to schema v20 that are both stale. Line 193 says `store/        - SQLite storage layer (Schema v20, WAL mode)`. Line 207 says `migrations.rs - Schema migration framework (v10-v20, including v19 FK cascade + v20 trigger)`. Actual `CURRENT_SCHEMA_VERSION = 22` in `src/store/helpers/mod.rs:92`. Schema v21 adds `parser_version` column (v1.28.0) and v22 adds `umap_x` / `umap_y` (v1.29.0). These were added after the triage wave-2 docs sweep.
- **Suggested fix:** Replace `Schema v20` → `Schema v22`, and `v10-v20, including v19 FK cascade + v20 trigger` → `v10-v22, including v19 FK cascade, v20 trigger, v21 parser_version, v22 UMAP coords`.

#### README.md does not document `cqs serve` — v1.29.0 feature invisible to users
- **Difficulty:** medium
- **Location:** `README.md` (entire file — no `cqs serve` section)
- **Description:** v1.29.0 shipped `cqs serve` as a flagship feature (CHANGELOG line 16-20 lists 4 separate `cqs serve` entries; ROADMAP line 9 highlights it; `src/cli/definitions.rs:730-748` defines the command). The README mentions "serve" only 4 times, all of them in context of `cqs watch --serve` (the daemon socket, not the web UI). Users looking at the README have no way to discover the interactive web UI, the 2D/3D toggle, the hierarchy view, the embedding cluster view, the `--port`/`--bind`/`--open` flags, or the `cqs index --umap` prerequisite for the cluster view.
- **Suggested fix:** Add a `## Web UI (`cqs serve`)` section between "Notes" and "Discovery Tools" (around line 226). Link to `docs/plans/2026-04-21-cqs-serve-v1.md` + `docs/plans/2026-04-22-cqs-serve-3d-progressive.md`. Document the four views, the `--port`/`--bind`/`--open` flags, and the `cqs index --umap` prerequisite for cluster view. Also add `cqs serve` to the "Claude Code Integration" command list starting at `README.md:468`.

#### README.md and CONTRIBUTING.md do not document `.cqsignore` — v1.29.0 feature missing
- **Difficulty:** easy
- **Location:** `README.md`, `CONTRIBUTING.md` (neither mentions `.cqsignore`)
- **Description:** v1.29.0 shipped `.cqsignore` as an opt-in exclusion mechanism layered on `.gitignore` (CHANGELOG line 21; `src/lib.rs:499-507` adds `wb.add_custom_ignore_filename(".cqsignore")`). 0 matches for `cqsignore` in README or CONTRIBUTING. Only the `Indexing` section mentions "Respects `.gitignore`" without noting that `.cqsignore` is also honored. Users won't discover they can exclude vendored minified JS / eval JSON / etc. without digging into the changelog.
- **Suggested fix:** In README.md `## Indexing` (line 587), add a sentence: "Also respects `.cqsignore` in the project root for cqs-specific exclusions (same syntax as `.gitignore`, layered on top). Use this for files committed to git but never worth indexing (vendored minified JS, generated fixtures, etc.)."

#### SECURITY.md wrong integrity-check default — says opt-out when actually opt-in
- **Difficulty:** easy
- **Location:** `SECURITY.md:22`
- **Description:** The doc says: `**Database corruption**: PRAGMA quick_check(1) on write-mode opens (opt-out via CQS_SKIP_INTEGRITY_CHECK=1). Read-only opens skip the check entirely`. Actual behavior per `src/store/mod.rs:960-962`: `let opt_in = std::env::var("CQS_INTEGRITY_CHECK").as_deref() == Ok("1"); let force_skip = std::env::var("CQS_SKIP_INTEGRITY_CHECK").as_deref() == Ok("1"); let run_check = opt_in && !force_skip && !config.read_only;` — the check is **skipped by default** and opt-in via `CQS_INTEGRITY_CHECK=1`. Comment at 955-959 confirms: "Opt-in via CQS_INTEGRITY_CHECK=1. The quick_check takes ~40s on WSL /mnt/c... For a rebuildable search index the risk/cost tradeoff favors skipping by default." The README env var table (line 722) has this correct (`CQS_INTEGRITY_CHECK | 0 | Set to 1 to enable PRAGMA quick_check on write-mode store opens`); SECURITY.md hasn't been updated.
- **Suggested fix:** Replace the bullet with: `**Database corruption**: Optional \`PRAGMA quick_check(1)\` on write-mode opens (opt-in via \`CQS_INTEGRITY_CHECK=1\`; disabled by default because the scan takes ~40s on slow filesystems). Read-only opens skip the check entirely — reads cannot introduce corruption and the index is rebuildable via \`cqs index --force\`.`

#### ROADMAP.md lists shipped `cqs serve` under "Parked"
- **Difficulty:** easy
- **Location:** `ROADMAP.md:174`
- **Description:** Line 174 says `- **Graph visualization** (`cqs serve`) — interactive web UI for call graphs, chunk types, impact radius. Spec: `docs/plans/graph-visualization.md`.` — but this is in the `## Parked` section. The same feature is marked shipped on line 184: `| v1.29.0 | **`cqs serve` + `.cqsignore` + slow-tests cron killed.**`. Additionally, the "Done" references `docs/plans/2026-04-22-cqs-serve-3d-progressive.md` as the governing spec, not `graph-visualization.md`. So (1) the parked entry should be removed entirely, and (2) the `graph-visualization.md` reference is to a superseded spec.
- **Suggested fix:** Delete line 174 (the `Graph visualization` bullet). If `docs/plans/graph-visualization.md` is no longer the working spec, either delete it or add a `SUPERSEDED` note pointing to `2026-04-22-cqs-serve-3d-progressive.md`.

#### CONTRIBUTING.md Architecture Overview missing 6 top-level source files / directories
- **Difficulty:** medium
- **Location:** `CONTRIBUTING.md:152-323`
- **Description:** The `src/` tree enumeration misses six items that exist on disk: `aux_model.rs` (HF repo id vs local path detection), `daemon_translate.rs` (CLI → batch command translation for daemon ping), `eval/` (eval harness code; see `src/eval/`), `fs.rs` (atomic replace helper from audit #981), `limits.rs` (env-var limit parsing), and `serve/` (v1.29.0 web UI with `assets/vendor/`). Compare `ls /mnt/c/Projects/cqs/src/` with CONTRIBUTING.md: every other top-level entry is described, these six are simply absent.
- **Suggested fix:** Add entries under the right sections of the architecture overview:
  - `aux_model.rs - HuggingFace repo id vs local path detection for model resolution`
  - `daemon_translate.rs - Translate CLI Commands to BatchCmd for daemon ping forwarding`
  - `eval/ - Eval harness: pool generation, ablation runs, per-category dashboards`
  - `fs.rs - atomic_replace helper (cross-fs rename fallback, canonicalized)`
  - `limits.rs - Env var limit parsing helpers (bounded numeric parsing)`
  - `serve/ - cqs serve web UI (v1.29.0): HTTP server, 4 views, embedded Cytoscape / Three.js / 3d-force-graph bundles`

#### `src/hnsw/build.rs:39` docstring points at nonexistent `cli/commands/index.rs`
- **Difficulty:** easy
- **Location:** `src/hnsw/build.rs:38-40`
- **Description:** Docstring reads: `/// # Production routing /// /// `build_hnsw_index()` in `cli/commands/index.rs` unconditionally uses /// `build_batched_with_dim()` with 10k-row batches for all index sizes.` — but `cli/commands/index.rs` does not exist. It's `src/cli/commands/index/build.rs:781` now (module was split into a directory). The `index/` module is a directory with `build.rs`, `gc.rs`, `stale.rs`, `stats.rs`, plus `mod.rs`. A grep for `pub(crate) fn build_hnsw_index` only hits `src/cli/commands/index/build.rs:781`.
- **Suggested fix:** Replace `cli/commands/index.rs` with `cli/commands/index/build.rs` in the docstring.

#### `.claude/skills/troubleshoot/SKILL.md` references nonexistent files and wrong default model
- **Difficulty:** easy
- **Location:** `.claude/skills/troubleshoot/SKILL.md:28,54`
- **Description:** Two stale refs still in the troubleshoot skill after the v1.27 audit wave-2 sweep partially touched it:
  1. Line 28: `Should contain `index.db` and `hnsw.bin`. If missing: `cqs init && cqs index`.` — there is no `hnsw.bin`. Actual HNSW files in `.cqs/` are `index.hnsw.data`, `index.hnsw.graph`, `index.hnsw.ids`, `index.hnsw.checksum` (per `SECURITY.md:71` pattern `.cqs/index.hnsw.*`). The skill was last touched when HNSW used a single-file layout that no longer matches reality.
  2. Line 54: `ls -la ~/.cache/huggingface/hub/models--intfloat--e5-base-v2/` — says the default model is E5-base, but the actual default is BGE-large (`CQS_EMBEDDING_MODEL | bge-large` per README line 706; `Default: BAAI/bge-large-en-v1.5` per PRIVACY.md line 32).
- **Suggested fix:** Line 28: replace `hnsw.bin` with `index.hnsw.*`. Line 54: replace the path with `~/.cache/huggingface/hub/models--BAAI--bge-large-en-v1.5/` or make the check model-agnostic (`ls -la ~/.cache/huggingface/hub/ | grep models--`).

#### `TODO(docs-agent): document this rule in CONTRIBUTING.md` unaddressed after landing
- **Difficulty:** easy
- **Location:** `src/cli/args.rs:553`, `src/cli/definitions.rs:637`
- **Description:** Both `IndexArgs::dry_run` (line 553) and the `Convert` subcommand's `dry_run` (line 637) carry an identical docstring block: `/// Audit P2 #38: per the CONTRIBUTING "Dry-Run vs Apply" rule, side-effect /// commands (`index`, `convert`) default to mutating; analyser commands /// (`doctor`, `suggest`) default to read-only and require `--fix`/`--apply` /// to mutate. TODO(docs-agent): document this rule in CONTRIBUTING.md.` The rule is still only captured in the source docstring — grep `Dry-Run\|dry-run\|dry run` in CONTRIBUTING.md returns 0 matches. Since this rule governs why `index`/`convert` behave opposite to `doctor`/`suggest`, it belongs in the user-facing contributor doc, not buried in clap arg attributes.
- **Suggested fix:** Add a short subsection in CONTRIBUTING.md (e.g., after "Adding a New CLI Command"): `### Dry-Run vs Apply — side-effect commands default to mutating\n\nSide-effect commands (\`cqs index\`, \`cqs convert\`) default to writing and expose \`--dry-run\` for preview. Analyser commands (\`cqs doctor\`, \`cqs suggest\`) default to read-only and require \`--fix\` / \`--apply\` to mutate. This split matches user expectation for each family.` Then remove the TODO lines from both `cli/args.rs:553` and `cli/definitions.rs:637`.

#### README.md `## Performance` section pinned to v1.27.0 eval file that predates two releases
- **Difficulty:** easy
- **Location:** `README.md:822-833`, `evals/performance-v1.27.0.json`
- **Description:** README line 823 says: `Measured 2026-04-16 on the cqs codebase itself (562 files, 15,516 chunks) with CUDA GPU (NVIDIA RTX A6000, 48 GB) on WSL2 Ubuntu. Embedder: BGE-large (1024-dim). SPLADE: ensembledistil (110M, off-the-shelf). Raw measurements: [\`evals/performance-v1.27.0.json\`](evals/performance-v1.27.0.json).` The chunk count `15,516` is pre-`.cqsignore` (`.cqsignore` dropped the corpus to `15,488` chunks per CHANGELOG line 21 and PROJECT_CONTINUITY.md line 57). The referenced file `performance-v1.27.0.json` is the only performance-*.json in evals/ — no v1.28.x or v1.29.0 refresh exists. Two point releases later the measurement is technically still the current "latest run," but the filename version pin misleads readers into thinking either the measurement is stale or there's a later file they can't find.
- **Suggested fix:** Rename the file to `performance-latest.json` (or copy it forward as `performance-v1.29.0.json` and update the link) so the version-in-filename convention matches the README version. Also update `(562 files, 15,516 chunks)` to reflect the current corpus if you re-run, or add a note: "Measurement from v1.27.0; retrieval pipeline unchanged through v1.29.0 so latencies still hold; chunk count is now 15,488 after `.cqsignore` landed in v1.29.0."

---

## API Design

#### `cqs --json project list` / `project remove` silently emit text, ignoring --json
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/project.rs:90-108,110-118` (`ProjectCommand::List` and `ProjectCommand::Remove` — no `--json` field, no `TextJsonArgs` flatten)
- **Description:** `cqs --json project list` and `cqs --json project remove foo` print plain text ("No projects registered.", "Removed 'foo'") even when the top-level `--json` flag is set. Verified with `cqs --json project list`: emits `No projects registered.` on stdout. Breaks programmatic consumers that assume `cqs --json <anything>` emits JSON. `cqs ref list` was already fixed to take `TextJsonArgs` (see `src/cli/commands/infra/reference.rs:54-58`); `project list`/`remove` were missed by the sweep. Same class of bug as post-v1.27.0 P1 #8 (`cqs --json project search` ignored `--json`), but on the sibling subcommands.
- **Suggested fix:** Change `ProjectCommand::List` and `ProjectCommand::Remove` to carry `#[command(flatten)] output: TextJsonArgs`, route `cli.json || output.json` into the handlers, and emit an envelope JSON payload (`{projects: [...]}` for list, `{status: "removed"|"not_found", name}` for remove) via `crate::cli::json_envelope::emit_json`. Mirrors what `cqs ref list` already does.

#### `cqs --json ref {add, remove, update}` silently emit text, ignoring --json
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/reference.rs:42-69` (`RefCommand::Add`, `RefCommand::Remove`, `RefCommand::Update` have no `TextJsonArgs`)
- **Description:** `cqs --json ref add foo /path` and `cqs --json ref remove bar` emit plain-text status (or a text `anyhow::Error`) even with `--json`. Verified: `cqs --json ref remove nonexistent-reference` prints `Error: Reference 'nonexistent-reference' not found in config.` on stderr. An agent driving config via JSON can't differentiate "already absent" (idempotent success) from "real error" (bad args) because there is no envelope. Same class as the `project list`/`remove` finding but on the reference-index mutations.
- **Suggested fix:** Add `#[command(flatten)] output: TextJsonArgs` to each of the three variants. Convert the handlers (`cmd_ref_add`, `cmd_ref_remove`, `cmd_ref_update`) to emit `{status, name, weight?}` envelopes when `--json` is set, and return a typed `not_found` error code via the shared `json_envelope::emit_json_error` path.

#### `cqs telemetry --reset --json` silently drops --json; handler prints text
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/telemetry_cmd.rs:520-578` (`cmd_telemetry_reset`) and dispatch at `src/cli/dispatch.rs:147-158`
- **Description:** `cqs telemetry --reset --json` prints `Archived 44912 events to telemetry_20260423_231842.jsonl` on stdout — plain text, not JSON. Verified live. The dispatch in `dispatch.rs:153-157` routes around the `--json` flag entirely: `if reset { cmd_telemetry_reset(&cqs_dir, reason.as_deref()) } else { cmd_telemetry(..., cli.json || output.json, all) }`. The reset path gets no json arg, and `cmd_telemetry_reset` has no json parameter. Same top-level `--json` swallow as post-v1.27.0 P1 #8 but on a mutating action.
- **Suggested fix:** Thread `cli.json || output.json` into `cmd_telemetry_reset` as a `json: bool` parameter. When true, emit `{archived_events, archive_path, lock_path}` via `emit_json`. Keep the existing text output for `json = false`. Update `dispatch.rs:153-157` to pass the flag.

#### `cqs notes list --check` is accepted by the CLI but dropped by the daemon batch dispatch
- **Difficulty:** easy
- **Location:** `src/cli/commands/io/notes.rs:51-64` defines `--check` on `NotesCommand::List`; `src/cli/args.rs:527-540` defines `NotesListArgs` (shared with batch) WITHOUT `--check`; `src/cli/batch/handlers/misc.rs:85-118` (`dispatch_notes`) has no `check` parameter and never calls `suggest::check_note_staleness`.
- **Description:** `cqs notes list --check` on the CLI populates a `stale_mentions` field per note by running `check_note_staleness` (see `src/cli/commands/io/notes.rs:544-593`). The daemon path — selected automatically when the daemon is running — parses into `NotesListArgs` which has only `warnings` and `patterns`. `BatchCmd::Notes` accepts `--check` as a parsed-but-unused flag via the `TextJsonArgs` carrier? No — `NotesListArgs` has no `check` field, so `echo 'notes --check' | cqs batch` will error "unexpected argument `--check`", AND `cqs notes list --check` routed to daemon produces a response without `stale_mentions` populated. Agents who rely on staleness info silently lose it when the daemon is up. Cross-refs the `NotesCommand::List` vs `NotesListArgs` split documented at `src/cli/args.rs:527-531`.
- **Suggested fix:** Add `pub check: bool` (with `#[arg(long)]`) to `NotesListArgs`. Extend `dispatch_notes` to accept the flag and call `cqs::suggest::check_note_staleness` when set, producing the same `stale_mentions: Option<Vec<String>>` shape as the CLI. Mirrors the wave-1 fix for `cqs stale --count-only` missing on batch (API-V1.25-2).

#### Daemon `dispatch_drift` / `dispatch_diff` emit `file` via `.display().to_string()` — CLI uses `normalize_path`
- **Difficulty:** easy
- **Location:** `src/cli/batch/handlers/misc.rs:277, 353, 365, 377` (all four uses of `e.file.display().to_string()`) vs `src/cli/commands/io/drift.rs:53` (`build_drift_output` uses `normalize_path`)
- **Description:** The daemon-side drift/diff handlers emit divergent file-path shapes compared to the CLI. CLI `cmd_drift` uses `normalize_path(&e.file)` (normalizes backslashes to forward slashes on Windows, strips UNC `\\?\` prefixes — the same invariant `cqs stale --json` and `cqs search --json` apply after P1 #19 in v1.27.0). Daemon `dispatch_drift` and `dispatch_diff` both use `e.file.display().to_string()`, which emits OS-native path separators. On Windows / WSL mixed projects, a consumer comparing `drift[].file` or `diff[].{added,removed,modified}[].file` to file paths from other `--json` commands gets mismatches (`src\\foo.rs` from daemon, `src/foo.rs` from CLI). Four call sites affected, same bug class.
- **Suggested fix:** Replace all four `e.file.display().to_string()` with `cqs::normalize_path(&e.file)` in `src/cli/batch/handlers/misc.rs`. Then factor the shared `DriftEntryOutput` / diff-entry serializers into `src/cli/commands/io/` so future field additions stay single-source.

#### `BatchCmd::Refresh` / `invalidate` has no CLI surface; agents can't invalidate daemon caches
- **Difficulty:** medium
- **Location:** `src/cli/batch/commands.rs:302-304` (`BatchCmd::Refresh`, visible alias `invalidate`) — no matching `Commands::Refresh` variant in `src/cli/definitions.rs:287+`
- **Description:** The daemon batch surface exposes a `Refresh` / `invalidate` verb that drops cached `hnsw`, `splade_index`, `call_graph`, `test_chunks`, `notes`, and refs — useful when a user edits the index out-of-band (e.g. `cqs model swap` that crashed mid-run, a manual SQLite patch, or a reference that got reindexed in another shell). It's only reachable via `echo 'refresh' | cqs batch` or direct socket send. There's no `cqs refresh` subcommand, no `cqs --invalidate` flag. Agents that know they mutated the index under the daemon have no first-class way to tell the daemon to re-read, so they fall back to `systemctl --user restart cqs-watch` or `CQS_NO_DAEMON=1`. This is a CLI/batch asymmetry directly relevant to agent ergonomics.
- **Suggested fix:** Add `Commands::Refresh` (and a clap `visible_alias = "invalidate"`) to the CLI definitions. Classify as `BatchSupport::Daemon` so it forwards to the daemon when present (its whole purpose is cache invalidation on the running daemon) and no-ops cleanly otherwise. Update `src/cli/dispatch.rs` with a one-line arm that forwards through `try_daemon_query` or bails cleanly ("no daemon running, nothing to refresh").

#### `cqs eval --limit` diverges from every other query command's `-n, --limit`
- **Difficulty:** easy
- **Location:** `src/cli/commands/eval/mod.rs:46-48` (`--limit` without `short = 'n'`)
- **Description:** Every query-returning subcommand uses `#[arg(short = 'n', long, default_value = "5")]` for its result cap (`search`, `callers`, `similar`, `scout`, `gather`, `explain`, `test-map`, `related`, `where`, `deps`, `onboard`, `plan`, `task`, `drift`, `blame`'s `-n, --commits`, `neighbors`, `project search`). `cqs eval --limit 20` works but `cqs eval -n 20` errors. `cqs train-pairs --limit` has the same gap. Agents that build commands from a template (`--limit N`) don't notice but humans doing `cqs eval -n 10` hit a surprise. Minor footgun.
- **Suggested fix:** Add `short = 'n'` to `EvalCmdArgs::limit` and `TrainPairs::limit`. One-token change per file, zero breakage since no caller uses `-n` today.

#### Pretty/compact JSON output drifts between CLI path and daemon path
- **Difficulty:** easy
- **Location:** `src/cli/dispatch.rs:786-790` (daemon response passed through `serde_json::to_string(other)` — compact) vs `src/cli/json_envelope.rs:242-251` (CLI `emit_json` uses `to_string_pretty` — multi-line)
- **Description:** `cqs --json impact-diff` emits compact single-line JSON when the daemon is running, pretty multi-line JSON when `CQS_NO_DAEMON=1`. Verified: `CQS_NO_DAEMON=1 cqs --json impact-diff` starts with `{\n  "data":...` while `cqs --json impact-diff` (daemon path) starts with `{"data":...`. Every other CLI subcommand has the same split. Agents / humans rely on output shape for diffing eval logs across runs; the two surfaces produce byte-different outputs for the same inputs. Not a bug for `jq` consumers but is a bug for snapshot tests and diff-review workflows.
- **Suggested fix:** In `try_daemon_query` (`src/cli/dispatch.rs:786-790`), use `serde_json::to_string_pretty` instead of `to_string` on the non-string `Value` arm, so the CLI-side output is pretty in both modes. Alternatively, switch `emit_json` to compact in both and pretty-print only when stdout is a TTY (matches `git`/`gh` conventions). Former is the zero-risk one.

#### `--expand` on `cqs search` vs `--expand-parent` on top-level `cqs <QUERY>` — same semantic, two names
- **Difficulty:** easy
- **Location:** `src/cli/args.rs:122-124` (`SearchArgs::expand`, flag `--expand`) vs `src/cli/definitions.rs:234-235` (`Cli::expand_parent`, flag `--expand-parent`)
- **Description:** The top-level `Cli` struct renamed the parent-context flag from `--expand` to `--expand-parent` to avoid colliding with `gather --expand <N>` (see the API-V1.22-3 comment). But `SearchArgs` (used by `BatchCmd::Search` AND by the flattened search args inside `Commands::Search` if it existed — actually `search` is not a named subcommand on CLI, only on batch) still exposes `--expand`. That means `echo 'search foo --expand' | cqs batch` works, `cqs search foo --expand` works in batch-mode only, but `cqs --expand-parent foo` is required at the top level. Three words for one concept across three slightly-overlapping surfaces. An agent batching `search foo --expand-parent` gets an error; batching `search foo --expand` works.
- **Suggested fix:** Rename `SearchArgs::expand` to `SearchArgs::expand_parent` with `long = "expand-parent"`, matching the top-level. Zero-risk rename — the tests in `src/cli/definitions.rs:1016-1038` already pin the top-level spelling; update the single batch-surface callsite. If backcompat is wanted, add `visible_alias = "expand"` and remove on v2.

#### `--depth` short-flag `-d` present on `cqs onboard`, absent on `cqs impact` and `cqs test-map`
- **Difficulty:** easy
- **Location:** `src/cli/args.rs:349-351` (`OnboardArgs::depth` has `short = 'd'`) vs `src/cli/args.rs:174-176` (`ImpactArgs::depth` — no short) and `src/cli/args.rs:322-324` (`TestMapArgs::depth` — no short)
- **Description:** Three commands expose `--depth <N>` with exactly the same semantic (search / BFS depth in the call graph) but only `onboard` has the `-d` short flag. Humans muscle-memory-typing `cqs impact foo -d 3` get "unexpected argument '-d' found". Post-v1.27.0 already caught the parallel issue of `--depth` having three different meanings (blame commits, onboard callee depth, test-map BFS); that was fixed on blame with the rename to `--commits`. The short-flag is the remaining inconsistency across the three that legitimately share semantics (all three are "call-graph search depth").
- **Suggested fix:** Either (a) add `short = 'd'` to both `ImpactArgs::depth` and `TestMapArgs::depth` for symmetry with onboard, or (b) remove `short = 'd'` from onboard for unified `--depth`-only. Option (a) is easier for humans; (b) is stricter for the no-short-flag-collisions stance elsewhere. Neither costs backcompat since all three are internal-only.

Summary: 10 API-design findings, all verified against live `cqs v1.29.0`. Dominant classes: (1) top-level `--json` silently dropped on non-flattened subcommands (`project list/remove`, `ref add/remove/update`, `telemetry --reset`), (2) CLI↔daemon-batch shape drift (`dispatch_drift` path separators, `NotesListArgs` missing `--check`, pretty-vs-compact JSON), (3) missing CLI surface for a daemon-only verb (`refresh`/`invalidate`), (4) flag-name/short-flag inconsistencies across commands that share semantics (`eval --limit` no `-n`, `--expand` vs `--expand-parent`, `--depth` short-flag asymmetry).

---

## Error Handling

#### `cli/commands/io/brief.rs::build_brief_data` silently swallows 3 consecutive store errors, producing zeroed counts with no signal
- **Difficulty:** medium
- **Location:** `src/cli/commands/io/brief.rs:59-75`
- **Description:** Three back-to-back store queries each do `.unwrap_or_else(|e| { tracing::warn!(...); <empty> })`:
  ```rust
  let caller_counts = store.get_caller_counts_batch(&names).unwrap_or_else(|e| {
      tracing::warn!(error = %e, "Failed to fetch caller counts");
      HashMap::new()
  });
  let graph = store.get_call_graph().unwrap_or_else(|e| {
      tracing::warn!(error = %e, "Failed to load call graph for test counts");
      std::sync::Arc::new(cqs::store::CallGraph::from_string_maps(...))
  });
  let test_chunks = store.find_test_chunks().unwrap_or_else(|e| {
      tracing::warn!(error = %e, "Failed to find test chunks");
      std::sync::Arc::new(Vec::new())
  });
  ```
  If any store query fails, `cqs brief <path>` returns a `BriefData { caller_counts: 0, test_counts: 0 }` with no flag indicating partial data. An agent reading the JSON cannot distinguish "this function genuinely has no callers or tests" from "the call-graph query failed". `BriefData` has no `warnings`/`partial` field. This is the anti-pattern called out in MEMORY.md (`.unwrap_or_default()` swallowing store errors).
- **Suggested fix:** Either (a) propagate with `?` so the command errors out and the agent knows to retry, or (b) add a `warnings: Vec<String>` field to `BriefData` / `BriefOutput` that records which store queries degraded, and surface it in both the JSON envelope and the terminal output.

#### `ci::run_ci_analysis` silently downgrades dead-code-scan failure into "0 dead" with no flag
- **Difficulty:** easy
- **Location:** `src/ci.rs:100-128`
- **Description:** The dead-code scan is wrapped in a `match store.find_dead_code(true) { Ok(...) => ..., Err(e) => { tracing::warn!(error = %e, "Dead code detection failed — CI will report 0 dead code (not 'scan passed')"); Vec::new() }}`. `CiReport` has `dead_in_diff: Vec<DeadInDiff>` but no `dead_scan_ok: bool` / `warnings` field. A CI consumer reading the JSON output cannot distinguish "no dead code in diff" from "dead-code scan crashed". The gate evaluation in `evaluate_gate` only looks at `risk_summary`, so a dead-code query failure never fails the gate — meaning a CI run whose index is unreadable will green-light on the dead-code dimension even though no scan happened. Given the tracing::warn! message already admits the ambiguity ("not 'scan passed'"), the JSON should expose the same signal.
- **Suggested fix:** Add a `dead_scan_ok: bool` (or `warnings: Vec<String>`) field to `CiReport`; set it `false` on the Err arm. Optionally short-circuit `gate.passed = false` on `dead_scan_ok == false` when threshold != Off, so CI fails loud on the query error rather than fooling the reviewer.

#### `dispatch::try_daemon_query` silently falls back to CLI when daemon output re-serialization fails
- **Difficulty:** easy
- **Location:** `src/cli/dispatch.rs:785-790`
- **Description:**
  ```rust
  let output = resp.get("output")?;
  let text = match output {
      serde_json::Value::String(s) => s.clone(),
      other => serde_json::to_string(other).ok()?,
  };
  return Some(text);
  ```
  Two silent `?` exits: (1) `resp.get("output")?` returns None if the daemon sent `status: ok` but no `output` field, (2) `serde_json::to_string(other).ok()?` drops the serialization error entirely. Both short-circuit to `None`, which in turn causes silent fallback to CLI re-execution — exactly the "silently swallow daemon-reported problems" class of bug that EH-13 fixed for error statuses. The daemon path gives 3-19ms responses; the CLI re-runs pay the ~2s startup cost. Silent fallback masks daemon JSON bugs and denies operators latency signal.
- **Suggested fix:** Replace both with explicit `tracing::warn!(stage = "parse", "Daemon ok response missing/unserializable output — falling back to CLI")` before returning `None` (the pattern already used for bad status at line 766-772 and error responses at line 798-803). No need to change the fallback semantics, just the silence.

#### `cli/commands/io/blame.rs::build_blame_data` silently suppresses callers fetch failure
- **Difficulty:** easy
- **Location:** `src/cli/commands/io/blame.rs:52-59`
- **Description:**
  ```rust
  let callers = if show_callers {
      store.get_callers_full(&chunk.name).unwrap_or_else(|e| {
          tracing::warn!(error = %e, name = %chunk.name, "Failed to fetch callers");
          Vec::new()
      })
  } else { Vec::new() };
  ```
  `BlameData` then returns `callers: Vec::new()` to the caller. User ran `cqs blame foo --show-callers`; they get zero callers back with no indication the store query failed. `BlameData` has no partial-data flag. Same class as brief.rs above — the explicit tracing::warn! acknowledges that the user-facing signal is missing but doesn't fix it.
- **Suggested fix:** Either propagate via `?` so `cqs blame --show-callers` errors out cleanly, or add a `callers_query_failed: bool` / `warnings: Vec<String>` field on `BlameData` / the emitted JSON, and print a `Warning: callers query failed` line on the text path so agents and humans see the same signal the journal sees.

#### `suggest::generate_suggestions` silently skips dedup against existing notes on store error
- **Difficulty:** easy
- **Location:** `src/suggest.rs:62-65`
- **Description:**
  ```rust
  let existing = store.list_notes_summaries().unwrap_or_else(|e| {
      tracing::warn!(error = %e, "Failed to load existing notes for dedup");
      Vec::new()
  });
  ```
  If the notes query fails, `existing` is empty, and the dedup `.retain()` below does nothing. `cqs suggest` then returns suggestions that duplicate notes the user already has — and nothing in the response signals that dedup was skipped. Because `cqs suggest --apply` reindexes notes, this can silently generate duplicate note rows when the store glitches mid-call.
- **Suggested fix:** Propagate the error (the caller already returns `Result<Vec<SuggestedNote>, StoreError>`, so switching to `?` is a one-character change), or add a `dedup_skipped: bool` field to the output struct. Propagation is the low-friction fix since this function is not user-agent facing directly.

#### `where_to_add::suggest_placement_with_options_core` silently drops pattern data on batch fetch failure
- **Difficulty:** easy
- **Location:** `src/where_to_add.rs:208-214`
- **Description:**
  ```rust
  let mut all_origins_chunks = match store.get_chunks_by_origins_batch(&origin_refs) {
      Ok(m) => m,
      Err(e) => {
          tracing::warn!(error = %e, "Failed to batch-fetch file chunks for pattern extraction");
          HashMap::new()
      }
  };
  ```
  On failure, every file suggestion emerges with empty `LocalPatterns` (no imports, empty error-handling string, empty naming convention, `has_inline_tests: false`). An agent using `cqs where` for placement guidance would follow empty patterns into wrong conventions without realizing the pattern extraction failed. `PlacementResult` has no `warnings` field to signal partial data.
- **Suggested fix:** Propagate with `?` (the function already returns `Result<PlacementResult, AnalysisError>`). If propagation is undesirable because the file-scoring result is still useful on its own, add a `patterns_degraded: bool` field to `PlacementResult` and set each `LocalPatterns` to `None`/a sentinel so consumers can tell empty-because-no-data from empty-because-we-didn't-look.

#### `cache::EmbeddingCache::stats` aggregates 5 silent per-query failures into a single lying `CacheStats`
- **Difficulty:** easy
- **Location:** `src/cache.rs:408-461`
- **Description:** Five separate `unwrap_or_else(|e| { tracing::warn!(...); 0 /* or None */ })` calls inside `stats()`:
  ```rust
  let total_entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM embedding_cache").fetch_one(...).await.unwrap_or_else(|e| { ...; 0 });
  let total_size: i64 = sqlx::query_scalar("SELECT page_count * page_size ...").fetch_one(...).await.unwrap_or_else(|e| { ...; 0 });
  let unique_models: i64 = sqlx::query_scalar(...).fetch_one(...).await.unwrap_or_else(|e| { ...; 0 });
  let oldest: Option<i64> = sqlx::query_scalar(...).fetch_one(...).await.unwrap_or_else(|e| { ...; None });
  let newest: Option<i64> = sqlx::query_scalar(...).fetch_one(...).await.unwrap_or_else(|e| { ...; None });

  Ok(CacheStats { total_entries: total_entries as u64, total_size_bytes: total_size as u64, ... })
  ```
  The return type is `Result<CacheStats, CacheError>` — but it can never be `Err` despite the Ok path silently masking up to five independent query failures. `cqs cache stats --json` would report `total_entries: 0` on a corrupt/locked DB and look indistinguishable from an empty cache. An operator debugging cache issues via `cqs cache stats` sees a lie. No `warnings`/`degraded` field on `CacheStats`.
- **Suggested fix:** Change all five to `?` propagation so `stats()` returns `Err(CacheError)` on any query failure (the simplest fix — callers already handle Err). If per-field tolerance is desired, convert the five fields to `Option<u64>` with explicit None on error and surface a `warnings: Vec<String>` field.

#### Daemon startup/periodic GC silently treats poisoned `gitignore` RwLock as "no matcher" — re-indexes ignored files
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:1737, 1945`
- **Description:**
  ```rust
  // 1737 (startup GC) and 1945 (periodic GC)
  let matcher_guard = gitignore.read().ok();
  let matcher_ref = matcher_guard.as_ref().and_then(|g| g.as_ref());
  run_daemon_startup_gc(&store, &root, &parser, matcher_ref);
  ```
  `RwLock::read()` returns `LockResult`; `.ok()` only yields `None` when the lock is **poisoned** (panic occurred while holding write lock). Poison is a serious invariant violation — but here it's silently converted to `matcher_ref = None`, which makes `run_daemon_startup_gc` / `run_daemon_periodic_gc` run with no gitignore filtering. The GC pass will treat previously-ignored files as candidates for re-indexing, re-populating the index with paths the user explicitly excluded. No tracing::warn! on the poison path. The only signal is the sudden explosion of chunk counts.
- **Suggested fix:** Replace `.ok()` with:
  ```rust
  let matcher_guard = match gitignore.read() {
      Ok(g) => Some(g),
      Err(poisoned) => {
          tracing::error!("gitignore RwLock poisoned, recovering — indexing may include previously-ignored paths this tick");
          Some(poisoned.into_inner())
      }
  };
  ```
  Using `into_inner()` keeps the previously-written matcher visible rather than running GC with no filter at all, and the error log makes the daemon journal show the poison event.

#### `convert::pdf.rs::find_pdf_script` and `convert::chm.rs`-style zip-slip: `.ok()` hides permission-denied from walker
- **Difficulty:** medium
- **Location:** `src/cli/commands/io/read.rs:232-235` (related pattern), and generally anywhere `sqlx` results are `.unwrap_or_else(..., HashMap::new())`
- **Description:** This is a *pattern finding*, not one site. Several user-facing commands (`read --focus`, `brief`, `blame`, `suggest`, `where`) follow the idiom:
  ```rust
  let batch_results = store.some_batch_query(...)
      .unwrap_or_else(|e| { tracing::warn!(error = %e, "..."); HashMap::new() });
  ```
  On store query failure, every downstream operation silently produces empty results. The pattern is ergonomic and logs a warn, but the output schema has no way to signal partial data to the agent consumer. An agent running `cqs where "add caching"` and seeing empty LocalPatterns doesn't know whether to re-run, widen the search, or abandon the task. Past audits (EH-10, EH-11) fixed the same class for the ingest pipeline by re-buffering on failure; the user-facing commands should follow suit by at least adding a `warnings` / `partial: bool` field to each output struct.
- **Suggested fix:** Introduce a project-wide convention: every struct serialized to JSON gets an optional `#[serde(skip_serializing_if = "Vec::is_empty")] warnings: Vec<String>` field. When an `unwrap_or_else(..., empty)` fires with a `tracing::warn!`, also push a one-line human string onto `warnings`. This is a mechanical sweep across ~6 commands (brief, blame, suggest, where, scout, task) and mirrors the `ScoutResult::search_degraded` + `expansion_capped` pattern that already exists (used at `src/cli/commands/search/gather.rs:199-207`). Adopting that pattern consistently would close this entire class.

#### `cagra::CagraIndex::delete_persisted` discards both `remove_file` errors silently, no log
- **Difficulty:** easy
- **Location:** `src/cagra.rs:1097-1102`
- **Description:**
  ```rust
  pub fn delete_persisted(path: &Path) {
      let _span = tracing::debug_span!("cagra_delete_persisted", path = %path.display()).entered();
      let _ = std::fs::remove_file(path);
      let _ = std::fs::remove_file(meta_path_for(path));
  }
  ```
  Doc-comment says "Best-effort — missing files are treated as success". The problem is that *real* I/O errors (EACCES, EBUSY, disk full) are indistinguishable from `NotFound` here — both are silently discarded. The only caller (`src/cli/store.rs:365`) uses this to clean up after a load failure before rebuilding; if the remove fails with EACCES, the next daemon restart hits the same corrupt-sidecar path and logs the same warn every boot indefinitely with no operator signal that cleanup itself is failing. `let _ =` here gives up more than necessary — `ErrorKind::NotFound` is the only expected failure mode that should be suppressed.
- **Suggested fix:**
  ```rust
  pub fn delete_persisted(path: &Path) {
      let _span = tracing::debug_span!("cagra_delete_persisted", path = %path.display()).entered();
      for p in [path.to_path_buf(), meta_path_for(path)] {
          match std::fs::remove_file(&p) {
              Ok(()) => {}
              Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
              Err(e) => tracing::warn!(path = %p.display(), error = %e,
                  "CAGRA cleanup failed — next rebuild may re-hit the same corrupt blob"),
          }
      }
  }
  ```

---

## Observability

#### `Reranker::rerank()` lacks entry span — the `rerank_with_passages()` wrapper has one but the shortcut path doesn't
- **Difficulty:** easy
- **Location:** `src/reranker.rs:160`
- **Description:** `Reranker::rerank` is the canonical public reranker API. It does NOT call `rerank_with_passages` — instead it fetches passages from results and calls `compute_scores` directly (see lines 177-181). Consequently the hot cross-encoder path has no span, while the seldom-used `rerank_with_passages` (line 197) has a `tracing::info_span!("rerank", count, limit, query_len)`. If a reranker regression shows up in journal logs (non-finite scores, timeout, session poison), the primary entry path is untagged and hard to correlate with the caller. A single tracing journal line "reranker took 850ms" can't be attributed to a specific search invocation without a parent span.
- **Suggested fix:** Add `let _span = tracing::info_span!("rerank", count = results.len(), limit, query_len = query.len()).entered();` at the top of `pub fn rerank` (line 165-166). Mirrors the span already present in `rerank_with_passages`.

#### `serve::build_chunk_detail` and `build_stats` lack `tracing::info_span!` — the other three `build_*` have them
- **Difficulty:** easy
- **Location:** `src/serve/data.rs:452` (`build_chunk_detail`), `src/serve/data.rs:933` (`build_stats`)
- **Description:** `serve/data.rs` exposes five public `build_*` functions invoked from axum handlers via `spawn_blocking`. Three of them (`build_graph:198`, `build_hierarchy:592`, `build_cluster:829`) open a `tracing::info_span!` at entry with useful fields. The remaining two — `build_chunk_detail` and `build_stats` — open no span and emit no tracing at all. `build_chunk_detail` runs 5+ distinct SQL queries including a blocking `rt.block_on(...)`. If any one fails, the `ServeError::Store` warn in `error.rs:50` fires without the chunk_id that triggered it, and `build_stats` has no trace at all if the 4 COUNT queries stall. Latency debugging for `/api/chunk/:id` and `/api/stats` is therefore blind.
- **Suggested fix:** In `build_chunk_detail`, add `let _span = tracing::info_span!("build_chunk_detail", chunk_id = %chunk_id).entered();` after the signature. In `build_stats`, add `let _span = tracing::info_span!("build_stats").entered();`. Matches the pattern used by the other three `build_*` functions.

#### `cmd_project` span doesn't record the subcommand dispatched (register / list / remove / search)
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/project.rs:75`
- **Description:** The entry span is `tracing::info_span!("cmd_project")` — no fields. All four subcommands (`Register`, `List`, `Remove`, `Search`) run inside the same undifferentiated span, so journal output for `cqs project search foo` and `cqs project list` is indistinguishable after the entry line. `Search` additionally initializes an `Embedder` and calls `search_across_projects` — the span should record which action was taken and, for Search, the query + limit. Currently nothing distinguishes a no-op `List` from a 5-second cross-project search in the journal.
- **Suggested fix:** Replace the single entry span with a match-scoped span, e.g. `let _span = match subcmd { ProjectCommand::Register { name, .. } => tracing::info_span!("cmd_project_register", name = %name).entered(), ProjectCommand::List => tracing::info_span!("cmd_project_list").entered(), ProjectCommand::Remove { name } => tracing::info_span!("cmd_project_remove", name = %name).entered(), ProjectCommand::Search { query, limit, .. } => tracing::info_span!("cmd_project_search", query = %query, limit).entered(), };` Alternatively keep one `cmd_project` span but record `action` and subcommand-specific fields via `span.record(...)`.

#### `hnsw::persist::verify_hnsw_checksums` uses format-interpolated `tracing::warn!` instead of structured field
- **Difficulty:** easy
- **Location:** `src/hnsw/persist.rs:136`
- **Description:** `tracing::warn!("Ignoring unknown extension in checksum file: {}", ext)` interpolates `ext` into the message. Every other warn in the file uses the structured `field = value` form. Structured fields enable filtering (`journalctl ... | jq 'select(.ext == "hnsw.foo")'`) — the interpolated form forces regex. This function is on the self-heal checksum verify path; when an attacker (or bit rot) manages to drop an unexpected extension in the checksum file, the operator has to grep the message text rather than query a field.
- **Suggested fix:** Change to `tracing::warn!(ext = %ext, "Ignoring unknown extension in checksum file");`.

#### `serve` axum handlers log entry but never log completion — no latency trace for API requests
- **Difficulty:** medium
- **Location:** `src/serve/handlers.rs:77-242` (`stats`, `graph`, `chunk_detail`, `search`, `hierarchy`, `cluster_2d`)
- **Description:** Each handler emits a single `tracing::info!` at entry (e.g. `"serve::stats"`, `"serve::graph"`). None wrap the body in a span and none log completion. Latency and error-path diagnosis for the web UI requires either (a) an external reverse proxy's access log, or (b) reading axum's tower middleware output, neither of which is configured by default in `serve::mod.rs`. When a user reports "the cluster view takes 10s to load", the journal shows no signal — not even whether the request hit `/api/cluster/2d` at all, let alone how long `build_cluster` took. Contrast with `cli/watch.rs:62` where `daemon_query` wraps the whole span and emits `cmd_duration_ms` on exit.
- **Suggested fix:** Wrap each handler body in a `tracing::info_span!("serve_<name>", <params>)` so the downstream `build_*` spans nest under it, and add an `.instrument(span)` on the `spawn_blocking` await if preserving span across tokio boundaries. Alternatively add `tower_http::trace::TraceLayer` to `build_router` in `serve/mod.rs:97` which gives latency + status code per request for free.

#### `classify_query` / `reclassify_with_centroid` lack entry span — routing decisions not traceable for a given query
- **Difficulty:** easy
- **Location:** `src/search/router.rs:561` (`classify_query`), `src/search/router.rs:1093` (`reclassify_with_centroid`)
- **Description:** `classify_query` (1549 lines of routing logic) decides which embedder path a query takes (DenseDefault vs DenseBase vs NameOnly vs DenseWithTypeHints). It's called from every search entry point. It has `tracing::info!(centroid_category, margin, "centroid filled Unknown gap")` at line 1116 but no entry span — so when a user asks "why did my query route to DenseBase?", the operator has to grep for the message text and has no correlation id back to the originating `cmd_search`/`cmd_scout`/etc. `resolve_splade_alpha` (P3 OB-NEW-1, triaged but still open — triage shows ✅ wave-1 which may be fixed already) was the sibling issue.
- **Suggested fix:** Add `let _span = tracing::info_span!("classify_query", query_len = query.len()).entered();` at the top of `classify_query`. At exit add `tracing::debug!(category = %classification.category, confidence = ?classification.confidence, strategy = ?classification.strategy, "Query classified");` so the full routing decision is one journal line. Same treatment for `reclassify_with_centroid` with `tracing::info_span!("reclassify_with_centroid").entered();`.

#### `verify_hnsw_checksums` silently returns errors on IO failure — no tracing line before wrapping
- **Difficulty:** easy
- **Location:** `src/hnsw/persist.rs:120-156` (`verify_hnsw_checksums`)
- **Description:** Every IO failure in this function (read checksum file, open data file, read through hasher) flattens `std::io::Error` into `HnswError::Internal(format!("Failed to read {}: {}", ...))` via `.map_err`. None of these emit a `tracing::warn!` before returning. When a real user hits a permission or FD-exhaustion problem during daemon startup self-heal (this function is called from `build_vector_index_with_config` line 447 on the hot path), the only signal reaching the journal is the eventual `tracing::warn!(error = %e, ...)` at the caller in `cli/store.rs:455` — which shows the flattened String, stripping the `io::ErrorKind`. For transient IO failures (NFS stall, filesystem readonly remount) operators can't tell the kind from the log alone.
- **Suggested fix:** Add `tracing::warn!(error = %e, path = %path.display(), kind = ?e.kind(), "verify_hnsw_checksums IO failure");` inline before each `.map_err(|e| HnswError::Internal(...))` so the `ErrorKind` reaches the journal even though the wrapped `HnswError` is a plain string.

---

## Test Coverage (adversarial)

#### TC-ADV-1.29-1: `normalize_l2` silently returns NaN/Inf for non-finite input — no test
- **Difficulty:** easy
- **Location:** `src/embedder/mod.rs:1023-1030` (definition). Tests live at `src/embedder/mod.rs:1200-1233`. Suggested new tests in the same module.
- **Description:** `normalize_l2` is called on every raw ORT output (`src/embedder/mod.rs:911`) and receives whatever the model produces. If any element is NaN, `norm_sq = v.iter().fold(0.0, |acc, &x| acc + x * x)` becomes NaN, the `norm_sq > 0.0` check is false, and the original NaN values are returned unchanged. If any element is +Inf, `norm_sq = Inf`, `inv_norm = 1.0/Inf = 0.0`, and the returned vector is all zeros — silently blanked. Neither case is tested today (existing tests cover unit-vector, 3-4-5, zero, empty). The HNSW search path has a NaN guard (`src/hnsw/search.rs:82`) but the `search_filtered` brute-force path, reranker path, and neighbors path all consume these embeddings without a guard — a degenerate ONNX output silently corrupts search results.
- **Suggested fix:** Add three tests in the existing `tests` module:
  - `test_normalize_l2_nan_propagates` — `normalize_l2(vec![1.0, f32::NAN, 0.0])` — pin current behavior (NaN out) OR change the contract to fail. Either way, don't leave it untested.
  - `test_normalize_l2_inf_collapses_to_zero` — `normalize_l2(vec![1.0, f32::INFINITY, 2.0])` — pin the current Inf → 0-vec collapse.
  - `test_normalize_l2_neg_inf` — same for `f32::NEG_INFINITY`.

#### TC-ADV-1.29-2: `embed_batch` does not validate ORT-returned tensor for NaN/Inf before returning `Embedding`
- **Difficulty:** medium
- **Location:** `src/embedder/mod.rs:903-914` (the pooled→normalized→Embedding::new path). No existing test covers the "model emits NaN" case.
- **Description:** `Embedding::try_new` rejects non-finite values, but `embed_batch` uses `Embedding::new(normalize_l2(v))` (line 911), which is the unchecked constructor. Combined with the `normalize_l2` NaN/Inf passthrough above, a broken ONNX model (observed in: quantization bugs, model-weight bit rot, corrupt model download with matching checksum, FP16 overflow on long inputs) produces `Embedding` values that flow through `embed_query` → query cache → HNSW search → brute-force scoring with no canary. `is_finite` is checked at HNSW search (line 82) — but the disk cache `QueryCache::put` (`src/cache.rs:1227`) stores the NaN embedding to disk, poisoning future queries across processes.
- **Suggested fix:** Add a test that monkey-patches a pooling result with a NaN element, then asserts `embed_batch` either (a) errors, or (b) produces a finite embedding — whichever is the chosen contract. Specifically:
  - `test_embed_batch_rejects_nan_pool_output` via a trait/mock or by poking a `normalize_l2` call path; assert `Result::Err(EmbedderError::InferenceFailed(_))` or similar.
  - In parallel, add `test_query_cache_put_rejects_non_finite_embedding` in `src/cache.rs` so even if embedder is misbehaving, the cache layer is a backstop.

#### TC-ADV-1.29-3: Daemon socket handler — zero adversarial tests for request shapes
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:160-406` (`handle_socket_client`). Tests start at line 2637, but none of them exercise `handle_socket_client`. Integration tests in `tests/daemon_forward_test.rs` only cover the CLI→daemon happy path (notes list, ping).
- **Description:** The daemon socket is the hot path for every agent query and has rich adversarial surface. None of the following are tested:
  - **1 MiB boundary**: request exactly 1,048,577 bytes should produce `"request too large"` error (logic at `src/cli/watch.rs:198-207`).
  - **Malformed JSON**: trailing garbage after a valid object, UTF-16 BOM prefix, JSON with NaN literals (`{"command":"ping","args":[]}NaN`), empty line, whitespace only.
  - **Missing `command` field**: `{"args":[]}` should produce `"missing 'command' field"` (line 304-312).
  - **Non-string args**: `{"command":"notes","args":[{}, null, 42]}` — rejects with `"args contains non-string elements"` (line 248-263). Today this is only covered structurally by code review.
  - **Oversized single arg**: `{"command":"search","args":["<500KB of base64>"]}` — within 1 MiB line but exhausts memory downstream.
  - **NUL byte in args**: the batch path validates NUL bytes (`src/cli/batch/mod.rs:579`) — but `handle_socket_client` relies on `dispatch_line` downstream to catch this; no integration test pins that boundary.
  - **Notes secret-redaction**: `{"command":"notes","args":["add","secret text"]}` — the log line should show `notes/add` (line 279-285), not the full arg. Regression risk that isn't test-pinned.
- **Suggested fix:** Create `tests/daemon_adversarial_test.rs`. Wire up a fixture that constructs `BatchContext` + runs `handle_socket_client` against a `UnixStream` pair (the existing `MockDaemon` in `tests/daemon_forward_test.rs` is the wrong shape — that's a mock daemon for the CLI; we need the reverse). Add one test per case above; assert response envelope matches expected error code or payload. A NUL-byte-in-args case should verify the client receives an invalid-input error rather than the command being executed with a mangled string.

#### TC-ADV-1.29-4: `parse_unified_diff` has no test for empty-file / whitespace-only hunk headers / duplicate `+++` lines
- **Difficulty:** easy
- **Location:** `src/diff_parse.rs:33-108` (definition), tests at `tests/diff_parse_test.rs` + `src/diff_parse.rs:110-237`.
- **Description:** Existing tests cover basic, new-file, deleted, binary, multiple hunks, count-omitted, empty, rename, u32-overflow, no-b-prefix. Missing:
  - **Two `+++` lines in a row without any hunk headers between them** — current code just overwrites `current_file` and does not warn; a diff like `+++ b/a.rs\n+++ b/b.rs\n@@ -1 +1 @@\n+x` will attribute the hunk to `b/b.rs` silently. Pin this behavior or reject.
  - **`@@` hunk header before any `+++` line** — dropped on the floor because `current_file = None`. No test.
  - **Only-whitespace diff input** (`"   \n\n\n"`) — currently returns empty Vec (via `lines()`) but not pinned.
  - **Hunk header with extra spaces inside** (`@@  -10,3  +10,5  @@`) — regex `\+(\d+)` requires exactly one space before `+`; the parser will silently drop the hunk. Not tested.
  - **CRLF-only line endings in middle of diff** (mixed with LF) — the `contains('\r')` check normalizes all `\r` to `\n`, but mid-hunk `\r` in the `+ line content` would double-normalize and change byte positions. Not tested.
- **Suggested fix:** Add to `tests/diff_parse_test.rs`:
  - `test_parse_unified_diff_double_plus_plus_line_uses_last` — pin last-wins behavior.
  - `test_parse_unified_diff_orphan_hunk_header_dropped` — hunk without preceding file is dropped.
  - `test_parse_unified_diff_hunk_header_extra_spaces` — pin current drop-on-floor behavior.
  - `test_parse_unified_diff_whitespace_only_input` — returns empty.

#### TC-ADV-1.29-5: `parse_notes_str` — no test for malformed TOML escapes, non-ASCII text, or oversized mentions array
- **Difficulty:** easy
- **Location:** `src/note.rs:325-348`. Existing tests cover happy path, clamping (not NaN — TC-ADV-6 in triage), empty file, stable IDs, MAX_NOTES truncation, proptest no-panic on 500-byte random input.
- **Description:** `parse_notes_str` accepts user-authored TOML. The proptest fuzz (`\\PC{0,500}`) won't hit these:
  - **A `[[note]]` with `mentions = [...]` containing 10,000+ strings**: each stored unchanged on `Note`. No per-note cap, no per-mentions cap. A malicious/mis-generated notes.toml can produce a Note with millions of mentions; `path_matches_mention` runs O(n_mentions × n_candidates) per query, DoS.
  - **`text = ""`** (empty string) — not rejected. The trimmed text is empty; hash of empty bytes is deterministic but conflicts with all other empty notes.
  - **`text = "\0\0\0"` (embedded NUL)** — accepted; when later written to a log line or daemon response, NUL may truncate downstream.
  - **`sentiment = "0.5"` (string instead of float)** — returns `NoteError::Toml` with a parse error message that leaks the raw TOML in the daemon error envelope. Not tested for redaction.
- **Suggested fix:** Add tests in `src/note.rs::tests`:
  - `test_parse_notes_str_huge_mentions_array` — 100k mentions on one note; assert it is parsed OR pin a cap. Recommend: cap mentions at e.g. 100 per note with warn.
  - `test_parse_notes_str_empty_text_rejected_or_kept` — pin behavior.
  - `test_parse_notes_str_nul_in_text` — assert NUL passes through verbatim; log/emit contract separately.

#### TC-ADV-1.29-6: HNSW `load_with_dim` — no test for id_map containing non-string JSON values
- **Difficulty:** easy
- **Location:** `src/hnsw/persist.rs:619-629` (id_map load). Existing tests at line 897+ cover oversized graph/data/id_map, missing checksum, dim mismatch, rebuild path.
- **Description:** The id_map is deserialized as `Vec<String>` via `serde_json::from_reader`. Several corrupt-but-parseable shapes are untested:
  - **id_map containing 10M zero-length strings** (each is `""` — 2 bytes in JSON). The file stays well under `MAX_ID_MAP_ENTRIES` (10M) cap but at 10M entries × avg 64 bytes overhead is 640 MB RAM. What happens when `id_map[5_000_000].clone()` hits a zero-string? That chunk never matches any chunk in SQLite — silent zero-result search, no warning.
  - **id_map with duplicate strings**: two entries `["chunk1", "chunk1"]` pointing to the same chunk id. The HNSW graph has two distinct nodes at position 0 and 1. Search returns `chunk1` twice with potentially different scores — duplicate result, breaks RRF downstream.
  - **id_map with strings containing embedded `\n` or `\0`**: survives deserialization, passes the `chunks_fts MATCH ?1` filter, but the `.hnsw.ids` JSON file round-trips — and the chunk_id becomes a lookup key in SQL. Injection surface if the id is later interpolated anywhere.
- **Suggested fix:** Add to `src/hnsw/persist.rs::tests`:
  - `test_load_rejects_duplicate_ids_in_id_map` — pin current behavior (duplicates accepted) or add a dedup check at load.
  - `test_load_rejects_empty_string_ids_in_id_map` — assert warn or error.
  - `test_load_rejects_nul_in_id_map_entry` — assert safety behavior.

#### TC-ADV-1.29-7: `embedding_slice` does not validate that decoded floats are finite — no test for NaN bytes in DB
- **Difficulty:** easy
- **Location:** `src/store/helpers/embeddings.rs:32-42`. Existing tests cover only size mismatch (3 cases).
- **Description:** `embedding_slice` validates byte length and casts to `&[f32]`. Any 4-byte sequence `0xFF 0xFF 0x7F 0x7F` is a valid NaN; `0x7F 0x80 0x00 0x00` is +Inf. If the SQLite embedding BLOB column is bit-rotted or written by a buggy embedder (see TC-ADV-1.29-2), `embedding_slice` silently returns NaN/Inf which flow directly into `score_candidate` (brute force path in `search_filtered`). `score_candidate` does have a NaN guard (test `score_candidate_nan_embedding_filtered` exists) — but the dot-product intermediates that feed it are computed every call. A whole-corpus NaN corruption from a single bad reindex produces zero results with no structured error.
- **Suggested fix:** Add to `src/store/helpers/embeddings.rs::tests`:
  - `test_embedding_slice_returns_nan_bytes_verbatim` — pin current passthrough behavior (the test author's choice: change contract or document it).
  - `test_bytes_to_embedding_nan_input` — same shape.
  - Recommend adding a `#[cfg(debug_assertions)]` sanity check that logs a warn-once if any decoded float is non-finite, since this is impossible under normal operation (the embedder always normalizes to unit length).

#### TC-ADV-1.29-8: `dispatch_line` shell_words tokenizer — no test for arg with ANSI escape sequences
- **Difficulty:** easy
- **Location:** `src/cli/batch/mod.rs:557-621`. Existing tests at line 2129+ cover NUL bytes in double-quoted args (P2 #51) and unbalanced quotes.
- **Description:** `dispatch_line` logs `args_len` (P3 #138 dropped the args_preview) but the full line passed downstream to `BatchInput::try_parse_from` is still parsed by clap with `args_preview`-style untrimmed tokens. If an agent sends `search "\x1b[2J\x1b[Hmalicious"` (ANSI clear screen + home cursor), the resulting error message flows to the client's terminal via tracing. Tested: NUL bytes (rejected). Untested: other C0 control chars (`\x07` BEL, `\x1b` ESC, `\x08` BS), `\r` as line separator within an arg, `\t` TAB inside a token.
- **Suggested fix:** Add to `src/cli/batch/mod.rs::tests`:
  - `test_dispatch_line_rejects_ansi_escape_in_arg` — pin either pass-through or rejection.
  - `test_dispatch_line_rejects_bel_in_arg` — same.
  - `test_dispatch_line_cr_in_arg_treated_as_arg_char_not_separator` — confirm single-line parsing still holds.

#### TC-ADV-1.29-9: `SpladeEncoder::encode` raw-logits path — no test for Inf-valued logits input
- **Difficulty:** easy
- **Location:** `src/splade/mod.rs:540-572` (encode, raw-logits branch). Tests at `src/splade/mod.rs:874+`.
- **Description:** Line 557: `pooled = logits.fold_axis(Axis(0), f32::NEG_INFINITY, |&a, &b| a.max(b))`. If `b` is +Inf, `a.max(+Inf) = +Inf`. Then line 564: `activated = (1.0 + val.max(0.0)).ln()` with `val = Inf` gives `activated = Inf`. Line 565: `Inf > self.threshold` → `true`, so the token is emitted with `Inf` weight. The resulting `SparseVector` then flows through `SpladeEncoder::search_with_filter`, which sums weighted dot products — Inf * anything = Inf, poisoning the entire score hash map. Silent corruption, no warning, no panic (the NaN branch actually filters via `> threshold == false`, but Inf passes the comparison).
- **Suggested fix:** Add to `src/splade/mod.rs::tests`:
  - `test_encode_rejects_inf_in_pooled_logits` (or sanitizes them) — pin whichever contract.
  - `test_encode_rejects_nan_in_pooled_logits` — the NaN path is "silently dropped" today; pin that or fail loudly.
  - Downstream: `test_splade_search_with_inf_weighted_sparse_vector` in `src/splade/index.rs`.

#### TC-ADV-1.29-10: `parse_unified_diff` called on 50 MB diff — no DoS test
- **Difficulty:** medium
- **Location:** `src/diff_parse.rs:33-108`, called from `src/cli/commands/graph/impact_diff.rs:39`, `src/review.rs:85`, `src/ci.rs:93`, `src/cli/batch/handlers/graph.rs:399`.
- **Description:** Upstream `MAX_DIFF_SIZE = 50MB` (`src/cli/commands/mod.rs:512`) caps stdin, but `parse_unified_diff` accepts any `&str` and builds a `Vec<DiffHunk>` with one allocation per hunk header matched. A 50 MB diff with a hunk header on every line (CRLF-normalized doubles memory briefly: `input.replace("\r\n", "\n").replace('\r', "\n")`) — hundreds of thousands of hunks, each allocating a `PathBuf` via `PathBuf::from(file.as_str())` (line 99). No cap on `hunks.len()`. `map_hunks_to_functions` has its own cap via `CQS_IMPACT_MAX_CHANGED_FUNCTIONS` (default 500), but that applies after `parse_unified_diff` has already built and returned the huge Vec. No test exercises the 50MB boundary or the "one hunk per line" worst case.
- **Suggested fix:** Add a test in `tests/diff_parse_test.rs`:
  - `test_parse_unified_diff_large_input_bounded` — construct 10 MB of `@@ +1,1 @@\n` lines with a leading `+++ b/foo.rs\n`, parse, assert Vec length matches and that memory usage stays bounded (e.g., under 100 MB wall-clock). Or, if a hard cap is desired, add `MAX_HUNKS` and pin it.
  - Currently `parse_unified_diff` also loses the performance feedback signal (no `tracing::info!` with hunk count after parse) so operators wouldn't see "10k hunks in one diff" in the journal.

## Summary

10 findings filed. Highest-impact gaps are (1) the `normalize_l2` NaN/Inf passthrough into disk cache and downstream scoring, (2) zero adversarial tests on the daemon socket handler (a production hot path handling untrusted JSON), and (3) the SPLADE raw-logits Inf propagation into sparse-vector score fusion. The diff parser has good coverage but misses some edge shapes that affect downstream review/impact commands.

---

## Robustness

#### RB-1: `timeout_minutes * 60` unchecked multiplication on env-var input
- **Difficulty:** easy
- **Location:** `src/cli/batch/mod.rs:343`, `src/cli/batch/mod.rs:378`
- **Description:** `let timeout = std::time::Duration::from_secs(timeout_minutes * 60);` — `timeout_minutes` is parsed directly from `CQS_BATCH_IDLE_MINUTES` / `CQS_BATCH_DATA_IDLE_MINUTES` via `.parse::<u64>().ok()`. A caller who sets `CQS_BATCH_IDLE_MINUTES=999999999999999999` lands in `u64` overflow (~307M year bound), silently wrapping to a small timeout value and evicting sessions on the very next tick — the opposite of what the user asked for. Debug builds panic, release silently wraps.
- **Suggested fix:** `Duration::from_secs(timeout_minutes.saturating_mul(60))` at both sites. Alternatively clamp `timeout_minutes` to a sane ceiling (e.g. `365 * 24 * 60`) in `idle_timeout_minutes()` / `data_cache_idle_timeout_minutes()`.

#### RB-2: `umap.rs` narrowing casts on row count / dim / id len without validation
- **Difficulty:** easy
- **Location:** `src/cli/commands/index/umap.rs:104-106,116`
- **Description:** The UMAP wire-protocol writes `(n_rows as u32).to_le_bytes()`, `(dim as u32).to_le_bytes()`, `(id_max_len as u32).to_le_bytes()`, `(id_bytes.len() as u16).to_le_bytes()`. `id_bytes.len() > u16::MAX` is checked at line 109 but `n_rows > u32::MAX` and `id_max_len > u32::MAX` are not. `dim` is bounded by model (fine) but `n_rows` is `buffered.len()` where `buffered` grows one entry per chunk from `store.embedding_batches`. A corpus with >4B chunks (unrealistic) silently truncates the row count in the header, causing the Python UMAP script to read fewer bodies than exist, misalign indices, and return wrong coordinates — silent data-corruption path rather than an error. Same pattern for `id_max_len` (max single-chunk-id length; plausible only if caller constructs pathological ids, but still an unchecked narrowing).
- **Suggested fix:** After line 93 add `anyhow::ensure!(n_rows <= u32::MAX as usize, "UMAP input too many rows: {n_rows} > u32::MAX");` and a matching guard for `id_max_len` next to the `id_bytes.len()` check that already exists.

#### RB-3: `serve/data.rs` negative `line_start` from DB silently clamped to 0 then cast to `u32`
- **Difficulty:** easy
- **Location:** `src/serve/data.rs:504`, and identical pattern in neighboring `Node`/`NodeRef` builders (`:787`, `:785-788`, etc.)
- **Description:** `line_start: r.get::<i64, _>("line_start").max(0) as u32,` — if `line_start` is somehow a negative i64 in the chunks table (corrupted index, migration bug, or a type-tree change), `max(0)` clamps to 0 and emits a "line 0" chunk in the served payload. If it's positive but > `u32::MAX`, the cast silently truncates. On the serve API this manifests as the frontend scrolling to the wrong line when the user clicks a node. No diagnostic.
- **Suggested fix:** Replace the shape with `let raw: i64 = r.get("line_start"); u32::try_from(raw).map_err(|_| ServeError::Internal(format!("chunk {id} has out-of-range line_start {raw}")))?`. Do the same at every call site (grep `line_start: r.get`). Alternatively emit a `tracing::warn!` with the chunk id and clamp, so stale data doesn't silently break the UI.

#### RB-4: `HierarchyDirection` query-string parsing panics on mixed-case / unicode input? — no: safely handled; skip
- **Skipped** (falsely suspected).

#### RB-5: `extract_l5k_regions` regex captures `.unwrap()` on group 0/1/2 — panic on concurrent regex corruption
- **Difficulty:** hard
- **Location:** `src/parser/l5x.rs:344-346,365`
- **Description:** `let routine_name = block.get(1).unwrap().as_str().to_string();` / `block.get(2).unwrap()` / `block.get(0).unwrap()`. The regex (`L5K_ROUTINE_BLOCK_RE` at line 323-325) has exactly two capture groups, so on a successful match groups 0, 1, and 2 must be present — this is safe against normal inputs. The only reachable panic is a `regex` crate bug where `captures_iter` yields a match with missing groups. No current evidence of such a bug, but the `.unwrap()` panic path is on user-content-derived input (L5X/L5K files in the indexing pipeline). If a corrupt file were to somehow produce a non-empty match-iterator whose capture layout is surprising, the whole indexer panics mid-walk. This is the only cluster of non-Mutex, non-fixed-size-try-into, non-regex-compile `.unwrap()`s in the production parser path.
- **Suggested fix:** Defensive — `let Some(routine_name) = block.get(1).map(|m| m.as_str().to_string()) else { tracing::warn!("L5K regex matched but group 1 missing — skipping"); continue; };`. Or accept the tiny risk and document it next to the regex (consistent with the `.expect("valid regex")` pattern used elsewhere). Low-impact, but a panic in the indexer aborts the whole `cqs index` / `cqs watch` pass.

#### RB-6: `chunk_count as usize` on u64→usize cast in CAGRA + CLI — silent truncation on 32-bit
- **Difficulty:** easy
- **Location:** `src/cagra.rs:606`, `src/cli/store.rs:344`, `src/cli/commands/index/build.rs:791,827`, `src/cli/commands/index/stats.rs:134,142-163`, `src/serve/data.rs` — widespread pattern
- **Description:** Many sites do `store.chunk_count()? as usize` where `chunk_count()` returns `u64`. On 32-bit targets this silently truncates at `usize::MAX == u32::MAX`, i.e. 4.3 billion chunks. cqs is 64-bit-only in practice (release targets are Linux x86_64, macOS ARM64, Windows x86_64), but there is no `#[cfg(target_pointer_width = "64")]` gate on the crate, and `cargo build --target i686-unknown-linux-gnu` is still mechanically buildable. Not a reachable panic on supported targets, but a silent wrap on pathological corpora in the (unsupported but buildable) 32-bit case.
- **Suggested fix:** Either gate the whole crate with `#[cfg_attr(not(target_pointer_width = "64"), compile_error!("cqs requires a 64-bit target"))]` in `src/lib.rs`, or replace the casts with `usize::try_from(chunk_count).map_err(StoreError::from)?`. Given the widespread pattern, the single-line crate-level gate is the cleaner fix.

#### RB-7: Channel `recv()` panics in indexing pipeline aren't routed to structured error
- **Difficulty:** medium (potentially no issue — verify)
- **Location:** survey didn't surface any `.recv().unwrap()` / `.send(...).unwrap()` in production — **likely no finding**, but the parallel-rayon pipeline (`src/cli/pipeline/parsing.rs`) uses `crossbeam_channel` with `?` propagation. No panic path confirmed. Skipping.
- **Suggested fix:** n/a

#### RB-8: `reranker.rs` batch_size × stride multiplication on inference output — no overflow guard
- **Difficulty:** easy
- **Location:** `src/reranker.rs:369,378`
- **Description:**
  ```rust
  let stride = if shape.len() == 2 { shape[1] as usize } else { 1 };
  // ...
  let expected_len = batch_size * stride;
  ```
  `shape[1]` is an `i64` from ORT. Cast to `usize` on a negative dimension wraps to a huge positive value (e.g. `-1 as usize = usize::MAX`). `batch_size * stride` then overflows and wraps. The subsequent `data.len() < expected_len` check still passes even on overflow (wrapped `expected_len` small), letting the function proceed with a broken stride. ORT shapes being negative is a spec-violating model but a malicious or corrupted `.onnx` file could have one.
- **Suggested fix:** After `shape[1] as usize`, add `if shape[1] < 0 { return Err(RerankerError::Inference(format!("negative output dim: {}", shape[1]))); }`. Then replace `batch_size * stride` with `.checked_mul(...)` and return `Inference` on None.

#### RB-9: `splade/mod.rs` `shape[N] as usize` on negative ORT dims — same pattern as RB-8
- **Difficulty:** easy
- **Location:** `src/splade/mod.rs:145,154,524,549,770-838` (multiple)
- **Description:** Six sites do `shape[N] as usize` on `i64` ORT shape values. Most are bounded by `shape.len() != 2/3` guards and subsequent `ArrayView2::from_shape` / `ArrayView3::from_shape` which would error on a misshape — but the immediate cast still wraps a negative dim silently into `usize::MAX`, then multiplies into `batch_size`/`seq_len`/`vocab` arithmetic *before* the `from_shape` check fires. A malicious SPLADE `.onnx` reports (batch=N, vocab=-1), vocab wraps to `usize::MAX`, `from_shape((batch, usize::MAX))` allocation attempt panics or OOMs the process (ndarray returns `ShapeError` rather than panicking in recent versions — safe — but the pattern is fragile).
- **Suggested fix:** Factor a helper `fn i64_dim_to_usize(d: i64, name: &str) -> Result<usize, SpladeError>` and use it at every `shape[N] as usize` site.

#### RB-10: `id_map.len() * dim * 4 * 2` in HNSW persist — unchecked mul on 64-bit (low risk on 32-bit)
- **Difficulty:** easy
- **Location:** `src/hnsw/persist.rs:647`
- **Description:** `let expected_max_data = id_map.len() * dim * std::mem::size_of::<f32>() * 2;` — bounded by `MAX_ID_MAP_ENTRIES = 10_000_000` and `dim` (nominal 1024). `10M × 1024 × 4 × 2 = 8.2 × 10^10` — fits `usize` easily on 64-bit. But the bound `dim` here comes from the loader's `dim` argument (caller-supplied from the Store's `model_info`). A pathological `model_info` with `dim = u32::MAX / 2` would overflow. On 64-bit targets this still fits (within usize), but on 32-bit it overflows silently and the subsequent `data_meta.len() as usize > expected_max_data` check would let a crafted file through.
- **Suggested fix:** `.checked_mul(dim)?.checked_mul(4)?.checked_mul(2)?` or `saturating_mul` with the same error path as the id_map guard above. Very small fix; defense-in-depth against future corpora with larger embedding dimensions.

Summary: 7 actionable findings (RB-1, RB-2, RB-3, RB-5, RB-6, RB-8, RB-9, RB-10 — 8 total; RB-4 and RB-7 skipped as false positives on deeper inspection). Most of the codebase has extensive saturating-arithmetic, `.ok_or_else` / `.get(i)?` patterns, and `.try_into()` with length guards already in place from prior audit rounds. The remaining issues are all narrow: (a) env-var multiplication overflows that nobody will hit in practice (RB-1); (b) unchecked u64→usize / i64→usize casts that are latent on 32-bit but not on supported 64-bit targets (RB-6, RB-10); (c) ORT shape[N]-as-usize casts that could wrap a negative dim into OOM/panic (RB-8, RB-9) — low probability but worth hardening for the security-critical ONNX surface.

---

## Scaling & Hardcoded Limits

#### [SHL-V1.29-1]: `pad_2d_i64` hardcodes pad-token-id = 0 — breaks non-BERT tokenizers
- **Difficulty:** medium
- **Location:** `src/embedder/mod.rs:813-814`
- **Description:** Code: `let input_ids_arr = pad_2d_i64(&input_ids, max_len, 0);` and `let attention_mask_arr = pad_2d_i64(&attention_mask, max_len, 0);`. The pad token id is hardcoded to `0`. This is correct for BERT-family tokenizers (bert-base, bge-large, e5-base all use `[PAD] = 0`), but RoBERTa/XLM-R use `<pad> = 1`, and custom tokenizers can use any id. The `pad_2d_i64` call assumes `0` unconditionally — a user wiring in a custom RoBERTa-tokenized ONNX via `[embedding] model_path = ...` would silently get padding tokens that the model interprets as `<s>` (start-of-sequence) tokens. Attention mask uses 0 correctly (masked = 0 is universal), but `input_ids` padding is tokenizer-specific. No retrieval of `tokenizer.get_padding().pad_id()` anywhere in the embedder.
- **Suggested fix:** Add `pad_id: i64` to `ModelConfig` (default 0, override via model registry / config), or read `tokenizer.get_padding().map(|p| p.pad_id).unwrap_or(0)` at session-init and cache on `Embedder`. Thread into `pad_2d_i64` call.

#### [SHL-V1.29-2]: `MAX_BATCH_LINE_LEN = 1 MB` hardcoded — blocks large-diff review via batch/daemon
- **Difficulty:** easy
- **Location:** `src/cli/batch/mod.rs:104`, used at `:1542`
- **Description:** `const MAX_BATCH_LINE_LEN: usize = 1_048_576;` rejects batch-mode lines above 1 MB with `"Line too long (max 1MB)"`. But the CLI path for `cqs review --stdin` / `cqs affected --stdin` accepts up to `MAX_DIFF_BYTES = 50 MB` (env `CQS_MAX_DIFF_BYTES`). So running the same workflow through the daemon (where the diff is quoted inline to a socket command) caps out 50× sooner than the direct CLI path. No env override. Error message doesn't mention how to bypass. With real-world PR diffs from monorepos easily reaching 5-10 MB, batch users silently hit this before the true limit.
- **Suggested fix:** Rename to `DEFAULT_MAX_BATCH_LINE_LEN`, add `batch_max_line_len()` reading `CQS_BATCH_MAX_LINE_LEN` (fallback 1 MB). Align the default with `MAX_DIFF_BYTES` or document why they differ. Error message should name the env var.

#### [SHL-V1.29-3]: `MAX_ID_MAP_SIZE = 100 MB` in `count_vectors` silently breaks large-corpus stats
- **Difficulty:** easy
- **Location:** `src/hnsw/persist.rs:732`
- **Description:** `const MAX_ID_MAP_SIZE: u64 = 100 * 1024 * 1024; // 100MB` in `count_vectors()`. If the id-map file exceeds 100 MB the function returns `None` silently (warns via `tracing::warn!` but `cqs stats` / health reports just see "unknown vector count"). With BGE-large id strings averaging ~50-60 bytes (e.g. `/long/path/to/file.rs:123:a1b2c3d4e5f6...`), 100 MB caps around ~1.7M chunks — well within the "we want to scale to 1M+" ambition. The hard-load path in `load()` has `MAX_ID_MAP_ENTRIES = 10_000_000` (10M entries) with a rationale; this stats-only path is tighter by an order of magnitude for no clear reason. No env override.
- **Suggested fix:** Either raise to match the hard-load cap (e.g. `10 * 1024 * 1024 * 1024` — 10 GB), or add `CQS_HNSW_ID_MAP_MAX_BYTES` env var, or compute from the load-path constant. At minimum, bump it; at best, rewrite `count_vectors()` to stream the JSON array count without holding the whole thing in memory.

#### [SHL-V1.29-4]: Onboard `MAX_CALLEE_FETCH = 30` / `MAX_CALLER_FETCH = 15` hardcoded, not env-configurable
- **Difficulty:** easy
- **Location:** `src/onboard.rs:30,33`
- **Description:** `const MAX_CALLEE_FETCH: usize = 30;` and `const MAX_CALLER_FETCH: usize = 15;`. These caps drop callee/caller content from the onboard reading list silently. For a codebase where a "central concept" (e.g. `parse_config`, `Store::query`) has 50+ callers, the user gets a truncated 15-caller list with no warning, no hint to raise the cap. No env override; no CLI flag; no knob at all. For small projects 15/30 is fine; for large projects / monorepos this is much too low. Contrast with `CALL_GRAPH_MAX_EDGES` which ships with `CQS_CALL_GRAPH_MAX_EDGES` env override.
- **Suggested fix:** Add `CQS_ONBOARD_MAX_CALLEES` / `CQS_ONBOARD_MAX_CALLERS` env vars, or better yet push them through `OnboardOptions` the way `ScoutOptions` threads `search_limit` / `search_threshold`.

#### [SHL-V1.29-5]: `task.rs` gather constants (`TASK_GATHER_DEPTH=2`, `TASK_GATHER_MAX_NODES=100`, `TASK_GATHER_LIMIT_MULTIPLIER=3`) hardcoded, no env
- **Difficulty:** easy
- **Location:** `src/task.rs:19-25`
- **Description:** All three `TASK_GATHER_*` constants are plain module-scope `const` with zero env/config/CLI plumbing. On a small project `max_nodes=100` is generous; on a 1M-chunk corpus the BFS gather phase truncates at 100 nodes and the task brief is tiny regardless of the user's `--limit`. Depth=2 / multiplier=3 are similarly fixed. No tracing warn on cap hit. The sibling `gather::GatherOptions` exposes these to callers, but `task()` ignores that and uses the hardcoded three.
- **Suggested fix:** Read `CQS_TASK_GATHER_DEPTH` / `CQS_TASK_GATHER_MAX_NODES` / `CQS_TASK_GATHER_LIMIT_MULTIPLIER` (with `OnceLock` caching like the other `CQS_*` helpers). Or plumb through `cmd_task` flags. Or accept a `TaskOptions` struct mirroring `GatherOptions`.

#### [SHL-V1.29-6]: `SCOUT_LIMIT_MAX = 50`, `SIMILAR_LIMIT_MAX = 100`, `RELATED_LIMIT_MAX = 50` hardcoded, no env override (unlike siblings in same file)
- **Difficulty:** easy
- **Location:** `src/cli/limits.rs:27,32,37`
- **Description:** Three `LIMIT_MAX` ceilings sit alongside `MAX_DIFF_BYTES`, `MAX_DISPLAY_FILE_SIZE`, `READ_MAX_FILE_SIZE`, `MAX_DAEMON_RESPONSE_BYTES` — every one of those has a resolver function (`max_diff_bytes()`, `max_display_file_size()`, etc.) reading its own `CQS_*` env var. The three `LIMIT_MAX` constants do not. They're re-exported via `src/cli/mod.rs:33` and consumed at 6 call sites (3 CLI, 3 batch). On a large corpus where an agent wants `cqs similar --limit 500` to see the full blast radius of a near-duplicate, it silently clamps to 100 with no warning and no way to override short of editing source. Inconsistent with the rest of the file.
- **Suggested fix:** Add `scout_limit_max()` / `similar_limit_max()` / `related_limit_max()` reading `CQS_SCOUT_LIMIT_MAX` / `CQS_SIMILAR_LIMIT_MAX` / `CQS_RELATED_LIMIT_MAX`. The `parse_env_usize` helper already exists in the same file.

#### [SHL-V1.29-7]: Health/suggest hotspot thresholds (`HOTSPOT_MIN_CALLERS=5`, `DEAD_CLUSTER_MIN_SIZE=5`, `HEALTH_HOTSPOT_COUNT=5`, `SUGGEST_HOTSPOT_POOL=20`) don't scale with corpus
- **Difficulty:** medium
- **Location:** `src/suggest.rs:14,18,21` and `src/health.rs:16`
- **Description:** On a 1M-chunk corpus, "5+ callers" is noise — every utility function hits that. The untested-hotspot detector surfaces hundreds-to-thousands of matches because the threshold doesn't scale. Similarly `HEALTH_HOTSPOT_COUNT=5` means `cqs health` always shows top-5 hotspots regardless of whether the corpus has 1k or 1M chunks. `SUGGEST_HOTSPOT_POOL=20` hard-limits pattern detection. None of these are env-configurable. The fix is either corpus-adaptive (thresholds scale with log2(chunk_count), mirroring the `cagra_itopk_max_default` pattern already in `src/cagra.rs:166`) or at minimum env-configurable.
- **Suggested fix:** Follow the `cagra_itopk_max_default` pattern — `HOTSPOT_MIN_CALLERS` = `(log2(n_chunks) * 0.6).clamp(5, 50)` or similar. At minimum, add `CQS_HOTSPOT_MIN_CALLERS`, `CQS_DEAD_CLUSTER_MIN_SIZE`, `CQS_HEALTH_HOTSPOT_COUNT` env vars.

#### [SHL-V1.29-8]: Risk-score thresholds (`RISK_THRESHOLD_HIGH=5.0`, `RISK_THRESHOLD_MEDIUM=2.0`) and blast_radius ranges (0..=2 / 3..=10) hardcoded, pub const but non-configurable
- **Difficulty:** medium
- **Location:** `src/impact/hints.rs:11,13` and `:148-152, 236-240`
- **Description:** Risk classification uses `score = caller_count * (1.0 - test_ratio)`; `>= 5` → High, `>= 2` → Medium. Blast-radius buckets `0..=2 → Low`, `3..=10 → Medium`, `>10 → High`. These were tuned for cqs-sized projects (~20k chunks). On a large monorepo where every module has 10-100 callers, the High/Medium/Low buckets collapse — almost everything is High. On a small script project, the buckets may never trigger beyond Low. No env override, no config section, no CLI flag. These values determine `cqs review` gate decisions (CI-blocking) so the wrong bucket silently changes the risk classification. The threshold is `pub const` (API-exposed) but nothing in the config schema scales it.
- **Suggested fix:** Add `[risk]` config section with `high_threshold` / `medium_threshold` / `blast_radius_low_max` / `blast_radius_high_min`, or env vars `CQS_RISK_HIGH` / `CQS_RISK_MEDIUM` / `CQS_BLAST_LOW_MAX` / `CQS_BLAST_HIGH_MIN`. Document the v1.29.0 defaults in `docs/notes.toml` so tuning is traceable.

#### [SHL-V1.29-9]: `DAEMON_PERIODIC_GC_INTERVAL_SECS=1800` and `DAEMON_PERIODIC_GC_IDLE_SECS=60` hardcoded, asymmetric with `DAEMON_PERIODIC_GC_CAP` which is env-overridable
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:887-888`
- **Description:** `DAEMON_PERIODIC_GC_CAP_DEFAULT` has a full env-resolver (`daemon_periodic_gc_cap()` reading `CQS_DAEMON_PERIODIC_GC_CAP`). But the two siblings — interval (30 min) and idle (60 s) — are hardcoded `const u64` with no resolver, no env, no config. A heavy-write environment (watch mode with continuous `cargo check`) might want a shorter interval so the daemon GC catches up; a laptop user on battery might want a longer one. Both knobs are already there in spirit but only one is plumbed. The CAP comment even says "Keeps each tick short" — so letting users tune the interval is the natural follow-through.
- **Suggested fix:** Mirror `daemon_periodic_gc_cap()` with `daemon_periodic_gc_interval_secs()` / `daemon_periodic_gc_idle_secs()` reading `CQS_DAEMON_PERIODIC_GC_INTERVAL_SECS` / `CQS_DAEMON_PERIODIC_GC_IDLE_SECS`.

#### [SHL-V1.29-10]: `convert/{html,mod}::MAX_FILE_SIZE = 100 MB` duplicated, hardcoded, no env override
- **Difficulty:** easy
- **Location:** `src/convert/html.rs:29` (`MAX_CONVERT_FILE_SIZE`) and `src/convert/mod.rs:363` (`MAX_FILE_SIZE` in `markdown_passthrough`)
- **Description:** Two separate `const X: u64 = 100 * 1024 * 1024;` declarations, same value, same semantic ("refuse to convert files above this size"). Previous audit P3 #106 extracted `DEFAULT_DOC_MAX_PAGES` and P3 #108 extracted `DEFAULT_DOC_MAX_WALK_DEPTH` to `src/limits.rs` with env overrides (`CQS_CONVERT_MAX_PAGES` / `CQS_CONVERT_MAX_WALK_DEPTH`). The per-file size cap got missed. A user converting a 150-MB HTML doc dump or a single large Markdown file silently fails with "exceeds 100 MB" — same class of silent-failure this file's top comment flags as the motivation for env-override plumbing. Also: `convert/webhelp.rs:117` has a separate `MAX_WEBHELP_BYTES = 50 MB` that's *not* the same constant but *is* similarly hardcoded.
- **Suggested fix:** Extract `DEFAULT_CONVERT_FILE_SIZE: u64 = 100 * 1024 * 1024` and `convert_file_size()` reading `CQS_CONVERT_MAX_FILE_SIZE` into `src/limits.rs` next to `doc_max_pages()` / `doc_max_walk_depth()`. Replace both hardcoded constants. Also thread `MAX_WEBHELP_BYTES` through the same helper or its own env var.

---

## Algorithm Correctness

#### `semantic_diff` sort by similarity has no secondary tie-breaker — non-deterministic "most changed" ordering across runs
- **Difficulty:** easy
- **Location:** `src/diff.rs:202-207` (and the parallel test helper at `src/diff.rs:298-303`)
- **Description:** `semantic_diff` populates `modified: Vec<DiffEntry>` by iterating a `HashMap` (process-seed-randomized order) and sorts with only one key:
  ```rust
  modified.sort_by(|a, b| match (a.similarity, b.similarity) {
      (Some(sa), Some(sb)) => sa.total_cmp(&sb),
      (Some(_), None) => std::cmp::Ordering::Less,
      (None, Some(_)) => std::cmp::Ordering::Greater,
      (None, None) => std::cmp::Ordering::Equal,
  });
  ```
  Two modified entries with identical similarity (e.g., both 0.73 — common for small, nearly-identical refactors) sort into arbitrary relative order across process invocations because `sort_by` is stable w.r.t. the (HashMap-derived, random) input order, not the data. `cqs diff` and `cqs drift` JSON output will reorder identical rows between runs, defeating diff-the-diff comparisons, breaking test determinism, and making eval-flake hard to reproduce. All other score-sorting sites in the codebase carry a full `(file, name, line_start)` tie-break cascade — this one was missed in the v1.25.0 wave-1 sweep that fixed the rest.
- **Suggested fix:** Replace the `Equal` fallbacks with a cascade on the stable identity fields `DiffEntry` already carries:
  ```rust
  fn cmp_entries(a: &DiffEntry, b: &DiffEntry) -> std::cmp::Ordering {
      match (a.similarity, b.similarity) {
          (Some(sa), Some(sb)) => sa.total_cmp(&sb),
          (Some(_), None) => std::cmp::Ordering::Less,
          (None, Some(_)) => std::cmp::Ordering::Greater,
          (None, None) => std::cmp::Ordering::Equal,
      }
      .then_with(|| a.file.cmp(&b.file))
      .then_with(|| a.name.cmp(&b.name))
      .then_with(|| a.chunk_type.cmp(&b.chunk_type))
  }
  ```
  Apply to both production (line 202) and the test at line 298 so they don't drift. Add a `proptest!`-style shuffling test that asserts the sort is stable across shuffled inputs.

#### `is_structural_query` keyword probe uses `format!(" {} ", kw)` and misses keywords at end-of-query
- **Difficulty:** easy
- **Location:** `src/search/router.rs:787-789`
- **Description:**
  ```rust
  STRUCTURAL_KEYWORDS
      .iter()
      .any(|kw| query.contains(&format!(" {} ", kw)) || query.starts_with(&format!("{} ", kw)))
  ```
  Covers keywords preceded by whitespace and surrounded by whitespace (via `" {} "`) or at the very start (via `"{} "`), but **not keywords at the end of the query**. Concrete failure trace for `"find all trait"` (3 words):
  - `is_identifier_query`: `"all"` is in `NL_INDICATORS` → returns false.
  - `is_cross_language_query`: no two language names → false.
  - `extract_type_hints`: "trait" isn't in the chunk-type hint table (which is phrases like "all traits") → none returned.
  - `is_structural_query`: `STRUCTURAL_PATTERNS_AC` doesn't match; keyword loop with `kw="trait"` → `query.contains(" trait ")` false (no trailing space), `query.starts_with("trait ")` false. **All keywords fail** → false.
  - `is_behavioral_query`: no behavioral verb word-match, no "code that"/"function that" → false.
  - `is_conceptual_query`: `words.len() == 3 <= 3`, `"all"` is NL-indicator match, `!is_structural_query` → **true**.
  - Routes to `Conceptual` (α=0.70), should have been `Structural` (α=0.90).

  Same pattern for `"show me all trait"`, `"find every impl"`, `"list all enum"`, `"all class"`, `"find enum"`, etc. — i.e., the common NL pattern where a user ends their query with the type they're looking for. This shifts SPLADE α from 0.90 → 0.70 for every such query (≈20% heavier SPLADE weight than intended on Structural), and the strategy enum shifts from `DenseWithTypeHints` → `DenseDefault`, bypassing the type-boost path entirely. Also allocates a `String` per (keyword × probe) iteration on every classify. The adjacent structural-pattern check uses Aho-Corasick — the keyword path should too.
- **Suggested fix:** Replace with a word-boundary check over the pre-computed `words` vec (same approach already used for `NEGATION_TOKENS`):
  ```rust
  pub fn is_structural_query(query: &str) -> bool {
      if STRUCTURAL_PATTERNS_AC.is_match(query) { return true; }
      // words is computed once upstream; pass it through instead of re-splitting
      let words: Vec<&str> = query.split_whitespace().collect();
      STRUCTURAL_KEYWORDS.iter().any(|kw| words.iter().any(|w| w == kw))
  }
  ```
  Add regression tests: `"find all trait"` → Structural, `"all class"` → Structural, `"find enum"` → Structural. No allocation, correct at EOL, matches the pattern the rest of the router uses.

#### `bfs_expand` processes BFS seeds in HashMap iteration order — non-deterministic `name_scores` when `max_expanded_nodes` cap is reached mid-expansion
- **Difficulty:** easy
- **Location:** `src/gather.rs:317-320` (seed enqueue from `name_scores.keys()`) and `src/gather.rs:326,338` (cap checks)
- **Description:**
  ```rust
  let mut queue: VecDeque<(Arc<str>, usize)> = VecDeque::new();
  for name in name_scores.keys() {
      queue.push_back((Arc::from(name.as_str()), 0));
  }
  while let Some((name, depth)) = queue.pop_front() {
      // ...
      if name_scores.len() >= opts.max_expanded_nodes && visited.len() > initial_size {
          expansion_capped = true;
          break;
      }
      // expand neighbors
  }
  ```
  `name_scores` is a `HashMap<String, ...>`, so `name_scores.keys()` iterates in seed-randomized order. When the BFS hits `max_expanded_nodes` mid-expansion (common on dense graphs — default `max_expanded_nodes` = 50 for onboard callers BFS, see `src/onboard.rs:165`), which seeds got expanded and which got cut off depends entirely on which order the iterator handed them out. Different runs of `cqs gather`, `cqs task`, `cqs onboard` on the same corpus/query produce different expanded graphs, different score maps, different final chunk lists after dedup+truncate. This is exactly the class of non-determinism the v1.25.0 tie-break sweep targeted, but it sits one layer up in the pipeline (BFS graph seeding, not result sorting).
- **Suggested fix:** Enqueue seeds in a deterministic order — easiest is a sort by `(initial_score desc, name asc)` before push:
  ```rust
  let mut seeds: Vec<(&String, (f32, usize))> =
      name_scores.iter().map(|(k, v)| (k, *v)).collect();
  seeds.sort_by(|a, b| {
      b.1.0.total_cmp(&a.1.0)               // higher score first
          .then_with(|| a.0.cmp(b.0))        // tie on name asc
  });
  for (name, _) in seeds {
      queue.push_back((Arc::from(name.as_str()), 0));
  }
  ```
  This respects the "process higher-scoring seeds first" intent (the old code happened to do this only by coincidence of HashMap hashing), and makes the cap-at-50 cutoff deterministic. Add a test that seeds two equally-scored entries, caps at a small `max_expanded_nodes`, and asserts the same `name_scores` on 100 re-runs.

#### `llm::summary::contrastive_neighbors` top-K selection sorts by score alone — non-deterministic neighbor choice when similarities tie
- **Difficulty:** easy
- **Location:** `src/llm/summary.rs:263,265,267`
- **Description:** Three sibling sorts all use `b.1.total_cmp(&a.1)` with no tie-break:
  ```rust
  candidates.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));           // line 263
  candidates.select_nth_unstable_by(limit - 1, |a, b| b.1.total_cmp(&a.1));  // line 265
  candidates.truncate(limit);
  candidates.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));           // line 267
  ```
  `candidates` is `Vec<(usize, f32)>` where `usize` is an index into `valid_owned`. When multiple candidates have identical similarity (common at low precision — f32 embeddings clamp to the same bit pattern for very close vectors, especially for L2-normalized embeddings over the same reindex cohort), `select_nth_unstable` can pick any of them, and the final neighbor set for a given seed is non-deterministic. This propagates into the prompt sent to the LLM for contrastive summary generation, so the *same* corpus + *same* seed chunk produces different summaries on different runs. Contrastive summary caching by content_hash then either caches the first random result forever (good) or wastes Batches API credits regenerating when the cache misses (bad — ~$0.38/run Haiku).
- **Suggested fix:** All three sort calls need the index as a secondary key. `candidates: Vec<(usize, f32)>` already carries the index:
  ```rust
  candidates.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
  candidates.select_nth_unstable_by(limit - 1, |a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
  candidates.truncate(limit);
  candidates.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
  ```
  Same cascade the rest of the codebase applies everywhere else.

#### `--name-boost` CLI arg accepts negative / >1 values — negative embedding weight, out-of-range fusion
- **Difficulty:** easy
- **Location:** `src/cli/args.rs:57-58` (arg declaration); `src/search/scoring/candidate.rs:286` (consumer)
- **Description:** CLI argument validation:
  ```rust
  #[arg(long, default_value = "0.2", value_parser = parse_finite_f32)]
  pub name_boost: f32,
  ```
  `parse_finite_f32` only rejects NaN/Infinity; any other `f32` value passes through. Consumer in `apply_scoring_pipeline`:
  ```rust
  (1.0 - ctx.filter.name_boost) * embedding_score + ctx.filter.name_boost * name_score
  ```
  A user calling `cqs search "foo" --name-boost 5.0` gets `(1.0 - 5.0) * embedding = -4.0 * embedding_score`, i.e., the embedding signal is **negated** — identical semantic matches get ranked last. Symmetrically, `--name-boost -1.0` gives `2.0 * embedding - 1.0 * name_score`, over-weighting embedding past its natural [0,1] range. The `.clamp(0.0, 1.0)` that the config-file path applies at `src/config.rs:370-371` is not mirrored on the CLI-flag path, so a config that looked safe can be overridden into a search-breaking regime via a stray flag. Most eval scripts set `--name-boost` explicitly, so a typo is one `bash` run away.
- **Suggested fix:** Replace the argument parser with a clamped variant. Either add a helper:
  ```rust
  fn parse_name_boost(s: &str) -> std::result::Result<f32, String> {
      let v = parse_finite_f32(s)?;
      if (0.0..=1.0).contains(&v) { Ok(v) } else {
          Err(format!("name_boost must be in [0.0, 1.0], got {v}"))
      }
  }
  ```
  and use `value_parser = parse_name_boost` at line 57. Or enforce the clamp at `SearchFilter` construction so config and CLI paths converge. Same fix applies to any other weight/threshold-style f32 flag.

#### `reranker::compute_scores_opt` — `batch_size * stride` unchecked multiplication hides shape errors; `data[i * stride]` can panic on overflow
- **Difficulty:** easy
- **Location:** `src/reranker.rs:368-387`
- **Description:**
  ```rust
  let stride = if shape.len() == 2 { shape[1] as usize } else { 1 };
  if stride == 0 { /* return error */ }
  let expected_len = batch_size * stride;              // <-- unchecked mul
  if data.len() < expected_len { /* return error */ }
  let scores: Vec<f32> = (0..batch_size).map(|i| sigmoid(data[i * stride])).collect();
  ```
  `shape[1]` is `i64` from ORT. The zero-guard landed after the prior audit (RB-8) but the negative-dim and overflow guards are still missing:
  - `shape[1] = -1` → `(-1_i64 as usize) = usize::MAX` (on 64-bit).
  - `batch_size * usize::MAX` wraps to a small value; `data.len() < expected_len` passes with that small wrapped value.
  - Inside the loop, `i * stride` also wraps, indexing `data` at an arbitrary position. If the wrapped index exceeds `data.len()`, **Rust bounds-checks and panics** in the middle of a hot inference call — aborting the entire search pipeline.
  A malicious / corrupted ONNX file (or a new reranker with an unusual output tensor layout) is the reachable source of a negative or pathologically-large `shape[1]`.
- **Suggested fix:** Guard the cast and the multiply:
  ```rust
  if shape.len() == 2 && shape[1] <= 0 {
      return Err(RerankerError::Inference(format!(
          "reranker output has non-positive dim 1: {}", shape[1]
      )));
  }
  let stride = if shape.len() == 2 { shape[1] as usize } else { 1 };
  if stride == 0 { /* existing error */ }
  let expected_len = batch_size.checked_mul(stride).ok_or_else(|| {
      RerankerError::Inference(format!(
          "reranker expected_len overflows: batch_size={batch_size} stride={stride}"
      ))
  })?;
  if data.len() < expected_len { /* existing error */ }
  ```
  Same pattern fixes the SPLADE six-site parallel in `splade/mod.rs` (see prior audit RB-9). The `data[i * stride]` indexing can stay as-is once the upstream `expected_len` check is sound.

#### `llm::doc_comments::select_uncached` sort has no tie-break beyond content length — non-deterministic selection when `max_docs` truncates
- **Difficulty:** easy
- **Location:** `src/llm/doc_comments.rs:222-229,242`
- **Description:**
  ```rust
  uncached.sort_by(|a, b| {
      let a_no_doc = a.doc.as_ref().is_none_or(|d| d.trim().is_empty());
      let b_no_doc = b.doc.as_ref().is_none_or(|d| d.trim().is_empty());
      b_no_doc.cmp(&a_no_doc)
          .then_with(|| b.content.len().cmp(&a.content.len()))
  });
  // ...
  uncached.truncate(uncached_cap);
  ```
  Two chunks with the same `has-doc` status and the same content-length byte count collide on the compare; `sort_by` is stable w.r.t. the input `uncached` vec's order, which is fed by a DB scan that may return duplicates-by-size in any order depending on index layout. When `--improve-docs --max-docs N` trips the truncate (line 242), which rows get documented vs skipped is non-deterministic across runs. For a Claude Batches API call (≈ $0.38 / run Haiku), that means the set of chunks that eat budget is non-reproducible. Between the enrichment re-run and the contrastive-summaries batcher this is the third "tie-break missing" site in `llm/*.rs`.
- **Suggested fix:** Append a stable tertiary key — chunk id is always unique and carried by `ChunkSummary`:
  ```rust
  .then_with(|| b.content.len().cmp(&a.content.len()))
  .then_with(|| a.id.cmp(&b.id))
  ```


---

## Extensibility

#### Adding a new CLI command requires coordinated edits across 5-7 files (dispatch fan-out)
- **Difficulty:** hard
- **Location:** `src/cli/definitions.rs:285-720` (Commands enum + BatchSupport match), `src/cli/dispatch.rs:67-134, 243-530, 552-609` (Group A + Group B + `command_variant_name`), `src/cli/batch/commands.rs:50-365, 404-531` (BatchCmd enum + `is_pipeable` + dispatch match), `src/cli/batch/handlers/*.rs`, `src/cli/args.rs` (args struct), `src/cli/commands/*/` (handler impl)
- **Description:** Adding one user-facing command like `cqs foo` forces edits in at least:
  1. `definitions.rs` — add `Commands::Foo { args, output }` variant
  2. `definitions.rs:787-891` — add variant to the `batch_support()` exhaustive match (`BatchSupport::Cli` or `::Daemon`)
  3. `dispatch.rs:67-134` or `:243-530` — add `Some(Commands::Foo {...}) => cmd_foo(...)` arm (Group A early-return or Group B store-using)
  4. `dispatch.rs:552-609` — add arm to `command_variant_name(&Commands)` telemetry mapper
  5. `batch/commands.rs:50-315` — add `BatchCmd::Foo { args, output }` variant
  6. `batch/commands.rs:317-366` — add variant to `is_pipeable()` exhaustive match
  7. `batch/commands.rs:404-531` — add dispatch arm that calls `handlers::dispatch_foo(...)`
  8. `batch/handlers/*.rs` — implement `dispatch_foo`
  9. `cli/args.rs` — add `FooArgs` struct (if shared CLI/batch)
  10. `cli/commands/*/foo.rs` — implement `cmd_foo` + re-export from `commands/mod.rs`
  11. Plus: `.claude/skills/cqs-foo/SKILL.md`, bootstrap portable skills list, `CLAUDE.md`/`README.md`/`CONTRIBUTING.md` agent docs, `CHANGELOG.md` — per MEMORY.md "New CLI commands need full ecosystem updates"

  Verified by reading `dispatch.rs:552-609` (`command_variant_name` is a 57-variant match that duplicates the `Commands` enum purely for a `tracing::info_span!` label — every new command must be added there with no compile-time link between the enum and the label), and `batch/commands.rs:317-366` (`is_pipeable` is a second exhaustive match separate from `batch_support`). Minimum edit points per new command: **5 exhaustive matches + 2 structs + 1 handler module + docs = ~10 places**.

- **Suggested fix:** Collapse the per-variant data onto the enum via trait + table. Introduce `trait Command { fn name(&self) -> &'static str; fn batch_support(&self) -> BatchSupport; fn is_pipeable(&self) -> bool; fn exec(&self, ctx: &CommandContext) -> Result<Output>; }`. Then `Commands` is just a dispatcher that forwards to trait methods. `command_variant_name` becomes `cmd.name()`. The `dispatch.rs` Group A/B match collapses to `cli.command.as_ref().map(|c| c.exec(&ctx))`. Alternatively, if trait-objects are too heavy, run all three classifiers through a single `fn metadata(&self) -> CommandMeta { name, batch_support, is_pipeable }` exhaustive match so adding a variant forces one classification decision, not four in four different files.

#### `where_to_add::extract_patterns` hardcodes Rust, TS/JS, Go custom logic instead of using `LanguageDef::patterns`
- **Difficulty:** medium
- **Location:** `src/where_to_add.rs:612-720` (three `Some(Language::…)` match arms for `Rust`, `TypeScript | JavaScript`, `Go`) vs the data-driven `Some(lang) => match pattern_def_for(lang)` fall-through
- **Description:** The function documents "Most languages use data-driven lookup via `pattern_def_for`. Three languages have custom logic: Rust (3-way visibility with `pub(crate)`), TS/JS (custom `require()` import matching), Go (name-based uppercase export detection)." But "custom logic" means adding a 4th language with similar needs (Kotlin `internal`/`public`/`private`, Swift `open`/`public`/`internal`/`private`/`fileprivate`, or Python `_`-prefix conventions) requires another dedicated match arm here, not a row in `languages.rs`. The table already has `VisibilityRule::SigStartsMajority` / `SigContainsMajority` / `SigContainsEitherMajority` — a 3-way `SigStartsTriage { a: "pub(crate)", b: "pub", else_: "private" }` variant would handle the Rust case. TS/JS `require(` detection could be one more `RegexImport { patterns: &["import ", "const ... = require("] }`. Go "uppercase-name = exported" could be `VisibilityRule::NameCase { if_upper: "exported", if_lower: "unexported" }`. Verified by reading `where_to_add.rs:550-600` (the `VisibilityRule` enum already covers 5 styles with clean data, then breaks the abstraction for three languages).
- **Suggested fix:** Extend `VisibilityRule` with `SigStartsTriage`, `NameCase`, and `RegexImportSet` variants, fold the Rust/TS/JS/Go logic into `LanguageDef::patterns` rows in `languages.rs`, and delete the three custom match arms in `extract_patterns`. Reduces the "to add language X: register grammar + queries + add a row" story from "unless X needs custom logic — then also edit where_to_add.rs" to a pure registry extension.

#### `LlmProvider` enum: adding a new provider requires editing 5+ sites with no compile-time glue
- **Difficulty:** medium
- **Location:** `src/llm/mod.rs:198-202` (enum), `:165-167` (hardcoded `API_BASE`, `MODEL`), `:282-296` (provider resolver), `:353-366` (`create_client` env-var match), `src/llm/batch.rs:65` (hardcoded `anthropic-version` HTTP header), `:103, 150, 229` (`is_valid_anthropic_batch_id` gate hardcoded in transport layer)
- **Description:** `LlmProvider::Anthropic` is the only variant, with a trailing `// Future: OpenAI, Local, etc.` comment that's been there long enough to rot. Adding `LlmProvider::OpenAI` requires: (1) adding the variant (easy), (2) editing the `match std::env::var("CQS_LLM_PROVIDER")` resolver to recognize the new name (line 282), (3) editing the `env_var = match llm_config.provider` in `create_client` (line 358), (4) implementing `BatchProvider` for a new struct (not for a generic `LlmClient` — the trait-obj story exists but `create_client` returns a concrete `LlmClient`, not `Box<dyn BatchProvider>` — line 366), (5) making `API_BASE` + `MODEL` + batch-id validation (`is_valid_anthropic_batch_id`) + HTTP headers (`anthropic-version`, `x-api-key`) provider-specific. Today those are file-level `const`s and hardcoded values in `batch.rs`. The `EX-31/EX-34` comment at `mod.rs:353` acknowledges this: "When adding providers, match on llm_config.provider here". That's precisely the pressure this finding is about — the plan to add providers is documented but every seam is single-provider-shaped. Verified by reading `create_client` (returns concrete `LlmClient`, not `Box<dyn BatchProvider>`) and `batch.rs:65` (HTTP `anthropic-version: 2023-06-01` header is a literal).
- **Suggested fix:** Make `create_client` return `Box<dyn BatchProvider>`; move `API_BASE` / `MODEL` / env-var-name / HTTP-header-shape / `is_valid_batch_id(&str) -> bool` into the `BatchProvider` trait (or an associated `ProviderMetadata` struct). Then `Anthropic` is one impl, `OpenAI` is a new file. `create_client` becomes `match provider { Anthropic => Box::new(AnthropicProvider::new(...)), OpenAI => Box::new(OpenAiProvider::new(...)) }` — one match, three lines per provider.

#### `AuxModelKind` preset registration duplicates the dispatch structure across a growing kind×preset matrix
- **Difficulty:** medium
- **Location:** `src/aux_model.rs:35-46` (`AuxModelKind` enum), `:136-148` (`config_from_dir` filename layout match on kind), `:166-173` (`preset` function dispatches to `splade_preset` or `reranker_preset`), `:177-200` (`splade_preset` hand-maintained), `:202-220` (`reranker_preset` hand-maintained), `:221-227` (`default_preset_name` separate match)
- **Description:** SPLADE and reranker today; a third aux kind (e.g. a future `Summarizer` ONNX model, or a dedicated `CodeEmbedder` preset pool separate from the main `ModelConfig` one) would need: new variant in `AuxModelKind`, new match arm in `config_from_dir` (for on-disk layout), new `foo_preset()` function, new arm in `preset()` dispatcher, new arm in `default_preset_name()`. Five parallel match-arms for one concept. And adding a preset within an existing kind (say a 3rd SPLADE variant) means editing the dedicated `splade_preset` function — contrast with `define_embedder_presets!` which makes adding a preset a one-row macro extension. Verified by reading `aux_model.rs:136-222` — five concept-coupled matches on `AuxModelKind` in the same file.
- **Suggested fix:** Apply the `define_embedder_presets!` macro pattern to aux models: a single `define_aux_presets!` table where each row is `(kind, name, aliases, repo_or_path_template, layout)`. The `preset()`, `default_preset_name()`, `config_from_dir()` layout decisions all derive from the table. Alternative: make `AuxModelConfig` a trait-object with `trait AuxModelProvider { fn name() -> &str; fn on_disk_layout(root: &Path) -> (PathBuf, PathBuf); fn presets() -> &[(&str, &str)]; }` and register per-kind in a `Vec<Box<dyn AuxModelProvider>>`.

#### `NotesListArgs` (batch) and `NotesCommand::List` (CLI) are two hand-maintained argument structs for the same command
- **Difficulty:** easy
- **Location:** `src/cli/args.rs:527-540` (`NotesListArgs` struct: `warnings`, `patterns` — no `check`) vs `src/cli/commands/io/notes.rs:49-65` (`NotesCommand::List { warnings, patterns, output, check }`)
- **Description:** The CLI `notes list` surface has a `--check` flag; the batch surface's shared-arg struct doesn't. Adding a new flag to `notes list` today requires updating both structs. The drift is already visible — the `--check` flag is silently dropped on the daemon path (already flagged in batch1-api-design as "NotesListArgs missing --check"). This is the same class as the explicitly-landed `SearchArgs` refactor (#947) — the rest of the notes surface never got unified. All other commands that have a shared `FooArgs` struct flattened into both `Commands::Foo { args: FooArgs, output: TextJsonArgs }` AND `BatchCmd::Foo { args: FooArgs, ... }` avoid drift by construction. Verified by reading `cli/args.rs:527-540` and `cli/commands/io/notes.rs:49-65` — the two structs are side-by-side, no shared source.
- **Suggested fix:** Lift `NotesListArgs` to carry every flag (`warnings`, `patterns`, `check`) then have `NotesCommand::List` flatten it the same way `Commands::Search { args: SearchArgs, output: TextJsonArgs }` does. Delete the inline fields on `NotesCommand::List`. One source of truth; new flags auto-propagate to both CLI and daemon-batch surfaces.

#### `cli/commands/infra/init.rs` hardcodes model sizes to a `dim >= 1024` heuristic
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/init.rs:42-50` (`let size = if cli.try_model_config()?.dim >= 1024 { "~1.3GB" } else { "~547MB" };`)
- **Description:** The download size hint is a 2-way dimension split — "dim>=1024 → 1.3GB, else → 547MB". Current presets (BGE-large 1024-dim=1.3GB, E5-base 768-dim=547MB) happen to sit on either side of that threshold. A custom 768-dim model distilled to 200MB reports "547MB". A 1024-dim quantized 600MB model reports "1.3GB". When the next preset ships at 1536-dim or 512-dim, the heuristic breaks again. Verified by reading `init.rs:42-50` — the two size strings and the boundary are literals, not sourced from `ModelConfig`.
- **Suggested fix:** Add an optional `approx_download_bytes: Option<u64>` field to `ModelConfig` / `define_embedder_presets!` row. For presets we ship, set it to the real figure; for user-supplied custom models, leave `None` and fall through to "(size unknown)". Removes the heuristic, stays a one-row edit per preset, and doesn't lie about custom-model sizes.

#### Tree-sitter query file naming is the only glue between `LanguageDef::chunk_query` and `queries/*.scm` — no compile-time check
- **Difficulty:** medium
- **Location:** `src/language/languages.rs` (every `definition_*` fn uses `chunk_query: include_str!("queries/<lang>.chunks.scm")` literal), `src/language/queries/*.scm`
- **Description:** Each language definition's `chunk_query` / `call_query` / `type_query` is wired via `include_str!("queries/<lang>.chunks.scm")`. A typo in the filename (`queries/rust.chunk.scm` missing the `s`) is a compile error — that part is fine. But: (1) if a language's queries directory gets renamed, nothing in the registry validates that every registered language has matching query files; the language silently returns empty chunks because the query compiles to a no-op. (2) There's no test that iterates `REGISTRY.all()` and ensures each `chunk_query` is non-empty (empty string compiles — `include_str!` of a 0-byte file compiles too). (3) Adding a new query kind (e.g., a hypothetical `docs_query` for extracting docstring content separately) would require editing `LanguageDef` and then fanning out `include_str!` calls across every `definition_*` function. Not a footgun today but the pressure mounts as query kinds grow (3 today: chunks, calls, types).
- **Suggested fix:** Either (a) add a startup self-test that iterates `REGISTRY.all()` and asserts every language with a grammar has a non-empty `chunk_query` — makes the "silently empty query" trap impossible, or (b) thread the query source through a single `fn load_query(lang_name: &str, kind: QueryKind) -> &'static str` that reads from a compile-time `phf`-style map keyed by (language, kind). New query kinds then require editing one enum + one table row per language that supports it, not every `definition_*` function.

#### Config schema: adding a `[foo]` section to `.cqs.toml` requires edits to 3-4 files with no shared pattern
- **Difficulty:** medium
- **Location:** `src/config.rs:138-190` (`Config` struct), `src/cli/config.rs:132-167` (`apply_config_defaults`), plus whichever consumer reads the field
- **Description:** Today `Config` is a flat struct with optional sub-sections for `scoring`, `splade`, `reranker`, `embedding`, and references. Adding a new `[router]` (or `[cache]`, or `[daemon]`) section requires: (1) new `RouterConfig` struct in `config.rs` with serde derives, (2) new optional field on `Config` with `#[serde(default)]`, (3) an `if let Some(ref x) = config.router { apply_router_config(...) }` chain in the consumer, (4) a new `apply_config_defaults` arm if any field overrides a CLI default. `apply_config_defaults` only knows about top-level scalar fields (`limit`, `threshold`, `name_boost`, `quiet`, `verbose`, `stale_check`) — every section-config is applied ad-hoc by its consumer. Verified by reading `cli/config.rs:132-167`: 6 hand-written `if cli.x == DEFAULT_X` arms, no way to register "apply my config section" without editing this file. Moreover, the sibling `DEFAULT_LIMIT` / `DEFAULT_THRESHOLD` / `DEFAULT_NAME_BOOST` constants must stay in lockstep with clap `#[arg(default_value = ...)]` attributes — that requirement is explicitly documented as a "SYNC REQUIREMENT" in the file header. Three-way sync between clap, the module const, and `apply_config_defaults` per defaulted option.
- **Suggested fix:** Introduce `trait ConfigSection { fn apply_to_cli(&self, cli: &mut Cli); }` or a method on `Config` that enumerates its own sections. Use clap's `ArgSource` instead of comparing against module-level `DEFAULT_*` constants so the sync requirement vanishes — `cli.args_seen[&Id::new("limit")] != ValueSource::DefaultValue` means "user set it explicitly". Each new section then registers itself via impl; `apply_config_defaults` collapses to `config.sections().for_each(|s| s.apply_to_cli(cli))`. Harder than a typical audit fix but eliminates a whole class of "I added a config field and it's silently ignored" bugs.

#### `aux_model::config_from_dir` and `reranker_preset` hardcode on-disk layout per kind — third-party forks or re-packaged bundles break silently
- **Difficulty:** easy
- **Location:** `src/aux_model.rs:136-148` (`config_from_dir` match on `AuxModelKind`), `:212-217` (reranker preset hardcodes `onnx/model.onnx` + `tokenizer.json`), `:188-198` (SPLADE preset hardcodes `model.onnx` + `tokenizer.json`)
- **Description:** `config_from_dir` has a 2-arm match on `AuxModelKind` encoding the on-disk layout convention: SPLADE bundles live as `{dir}/model.onnx` + `{dir}/tokenizer.json`; reranker bundles follow the HF cross-encoder convention `{dir}/onnx/model.onnx` + `{dir}/tokenizer.json`. A user whose SPLADE bundle ships as `{dir}/onnx/model.onnx` (to match the HF layout convention) or whose reranker repo skipped the `onnx/` subdirectory (some HF reranker forks do) gets a silent "file not found" much later at load time — not at config resolution, where the error message would be actionable. And the hardcoded path-template in each `*_preset` function means adding a preset that ships with a different layout (e.g. `tokenizer.json.gz` or `tokenizer.model` for sentencepiece) requires editing both the preset fn and `config_from_dir`.
- **Suggested fix:** Move `onnx_path` + `tokenizer_path` into a per-preset field on `AuxModelConfig` (they're already there) and have `config_from_dir` take them as parameters rather than deriving them from `kind`. Then the kind enum becomes just a scoping tag, and presets / user overrides can name their files freely. Removes the two-layer hardcoding (kind → layout + preset → layout) so a 3rd aux kind doesn't need to re-decide.

Summary: 9 extensibility findings. Dominant pressure: the CLI→batch dispatch fan-out (one command touches 5 exhaustive matches in 4 files), plus hardcoded-per-language custom logic that should have rolled into `LanguageDef`, plus "trait exists but only-one-impl" provider patterns (`LlmProvider`, `AuxModelKind`) that name the extensibility point without actually paying for it.

---

## Platform Behavior

#### PB-V1.29-1: `cqs context`/`cqs brief` fail on Windows when user types path with backslashes
- **Difficulty:** easy
- **Location:** `src/cli/commands/io/context.rs:28,115` + `src/cli/commands/io/brief.rs:40-42`
- **Description:** `cmd_context` and `cmd_brief` pass raw CLI `path: &str` through to `store.get_chunks_by_origin(path)`, which binds `WHERE origin = ?1`. The DB only ever stores forward-slash origins (enforced by `normalize_path` + the `debug_assert!(!origin.contains('\\'))` at `staleness.rs:589-592`). On Windows, a user / agent running `cqs context src\foo.rs` will get `"No indexed chunks found"` even when the file is indexed. `cmd_reconstruct` at `reconstruct.rs:32-39` proves the correct pattern (it calls `cqs::normalize_path` first); `cmd_context` and `cmd_brief` were missed.
- **Suggested fix:** Normalize the user-supplied path before lookup:
  ```rust
  let path_norm = cqs::normalize_slashes(path);
  let chunks = store.get_chunks_by_origin(&path_norm)?;
  ```
  Apply the same in `cmd_brief`, `dispatch_context`, and anywhere else that forwards CLI path to `get_chunks_by_origin[s_batch]`.

#### PB-V1.29-2: Watch SPLADE encoder passes Windows `file.display()` to `get_chunks_by_origin` — silent no-op
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:1083-1085`
- **Description:**
  ```rust
  for file in changed_files {
      let origin = file.display().to_string();
      let chunks = match store.get_chunks_by_origin(&origin) { ... };
  ```
  On Windows `PathBuf::display()` emits the verbatim `\\?\C:\...` prefix for canonicalized paths AND backslash separators. DB origins are stored as relative + forward-slash via `normalize_path`. So `get_chunks_by_origin(&origin)` returns `Ok(vec![])` on Windows, `encode_splade_for_changed_files` silently produces an empty batch, and `cqs watch --serve` never updates the SPLADE index for any modified file on Windows. Silent correctness failure.
- **Suggested fix:** Use the project-relative, forward-slash form consistently. Use `cqs::normalize_path(file)` (or the already-computed `rel_path` from the caller's loop) rather than `file.display().to_string()`. Re-verify with `RUST_LOG=debug` after fix on a Windows/WSL+DrvFS test path.

#### PB-V1.29-3: `chunk.id` prefix-strip uses `abs_path.display()` — breaks on Windows verbatim + backslash paths
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:2432-2434`
- **Description:**
  ```rust
  if let Some(rest) = chunk.id.strip_prefix(&abs_path.display().to_string()) {
      chunk.id = format!("{}{}", rel_path.display(), rest);
  }
  ```
  `chunk.id` format is `{path}:{line_start}:{content_hash}` where `{path}` is assigned during parsing. If the parser produced a forward-slash id (matching the rest of the codebase's normalization), `strip_prefix(abs_path.display())` fails on Windows (where display emits backslashes / `\\?\` verbatim) and the id keeps the absolute path. Chunks then end up with ids that don't match the relative-path ids seen everywhere else in the index, breaking `cqs read`, `cqs context`, `cqs callers` joins — silent data integrity drift on incremental indexing. The same file full-re-indexed produces correctly-prefixed ids, so stale/fresh chunks mix.
- **Suggested fix:** Normalize both sides through `cqs::normalize_path`:
  ```rust
  let abs_str = cqs::normalize_path(&abs_path);
  if let Some(rest) = chunk.id.strip_prefix(&abs_str) {
      chunk.id = format!("{}{}", cqs::normalize_path(&rel_path), rest);
  }
  ```
  Add a regression test that covers `chunk.id` containing backslashes + `\\?\` prefix.

#### PB-V1.29-4: `init` writes `.gitignore` with LF-only, breaks `git status` on Windows `core.autocrlf=true`
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/init.rs:36-40`
- **Description:** Finding PB-V1.25-8 from the v1.25.0 triage is still pending:
  ```rust
  std::fs::write(
      &gitignore,
      "index.db\nindex.db-wal\n...\n",
  )
  ```
  On a Windows git checkout with `core.autocrlf=true` (default on Git-for-Windows), `git status` immediately shows `.cqs/.gitignore` as modified because Git re-writes it with CRLF endings. The file is not even under source control (lives in `.cqs/`) but agents running on Windows get noise in `cqs blame` / `cqs diff` on any working-tree inspection.
- **Suggested fix:** Either (a) write platform-native line endings via `#[cfg(windows)]` replacing `"\n"` with `"\r\n"`, or (b) avoid autocrlf detection via a `.gitattributes` sibling in `.cqs/` marking `* -text`. Option (a) is the least-surprising fix.

#### PB-V1.29-5: JSON path fields emit Windows backslashes — breaks cross-platform agent consumers
- **Difficulty:** easy
- **Location:** `src/cli/batch/handlers/misc.rs:277,353,365,377`
- **Description:** `dispatch_drift` + `dispatch_diff` serialize result file paths as `e.file.display().to_string()`:
  ```rust
  "file": e.file.display().to_string(),
  ```
  On Windows, this emits `src\foo.rs` in the JSON envelope. The rest of the codebase normalizes to forward slashes via `cqs::normalize_path` / `serialize_path_normalized`. An agent that reads `cqs drift --json` then uses the `file` field for a follow-up `cqs impact` / `cqs read` call will feed a backslash path into `get_chunks_by_origin` — which (per PB-V1.29-1) returns nothing.
- **Suggested fix:** Use the existing helper. Either inline `cqs::normalize_path(&e.file)` at each site, or have the structs use `#[serde(serialize_with = "cqs::serialize_path_normalized")]` on their `PathBuf` fields.

#### PB-V1.29-6: Hardcoded `/mnt/` WSL check in `hnsw/persist.rs` + `project.rs` — ignores custom `wsl.conf automount.root`
- **Difficulty:** easy
- **Location:** `src/hnsw/persist.rs:86-87` + `src/project.rs:85-86` + `src/config.rs:445-451`
- **Description:** Three independent WSL-mount checks spot-probe the default `/mnt/` prefix:
  ```rust
  if crate::config::is_wsl()
      && dir.to_str().is_some_and(|p| p.starts_with("/mnt/"))
  ```
  WSL allows users to customize the automount root in `/etc/wsl.conf` (e.g. `root = /windows/`). Only `cli/watch.rs::is_under_wsl_automount` parses `wsl.conf` to handle this (PB-3 fix). The HNSW advisory-locking warning, project-registry locking warning, and config-permission-skip branch all silently miss non-default automount roots. On such a system the WSL file-locking advisory warning never fires and the permission-check spams warnings at users whose NTFS mount lives at `/windows/c/...`.
- **Suggested fix:** Lift `is_under_wsl_automount` out of `cli/watch.rs` into `cqs::config` and use it in all four sites (including the existing wsl.conf parser). Alternatively, detect DrvFS specifically via `statfs` magic number (9P=0x01021997 / DrvFS has its own signature).

#### PB-V1.29-7: `EmbeddingCache::open` / `QueryCache::open` propagate `set_permissions(0o700)` on WSL `/mnt/c/` — cache open can fail spuriously
- **Difficulty:** easy
- **Location:** `src/cache.rs:73-80, 1002-1009`
- **Description:**
  ```rust
  if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent)?;
      #[cfg(unix)]
      {
          use std::os::unix::fs::PermissionsExt;
          std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
      }
  }
  ```
  The `?` propagates the permissions-set failure. On WSL DrvFS (`/mnt/c/`), NTFS doesn't honor POSIX mode bits — `set_permissions` returns `EINVAL` or succeeds as no-op depending on kernel/DrvFs version. If the cache file lives on `/mnt/c/` (because `dirs::home_dir()` points at a Windows HOME under WSL) the `?` kills the entire cache-open with a "permission denied" error and the binary falls back to un-cached queries (or crashes an indexing pipeline). Same pattern was already fixed in PB-V1.25-15 for the daemon socket (`.ok()` → explicit warn). Asymmetric.
- **Suggested fix:** Downgrade to best-effort with a warn-on-failure. Mirror the pattern at `cache.rs:145-150`:
  ```rust
  if let Err(e) = std::fs::set_permissions(parent, ...) {
      tracing::warn!(path = %parent.display(), error = %e,
          "Failed to tighten cache parent dir permissions (WSL DrvFs / NTFS?); continuing");
  }
  ```

#### PB-V1.29-8: `HF_HOME` / `HUGGINGFACE_HUB_CACHE` env lookup doesn't honor Windows `%LOCALAPPDATA%` default
- **Difficulty:** medium
- **Location:** `src/cli/commands/infra/doctor.rs:858-868` + `src/splade/mod.rs:957` + `src/aux_model.rs:181,188,446,528`
- **Description:** All of these hardcode `~/.cache/huggingface/...`:
  ```rust
  dirs::home_dir().map(|h| h.join(".cache/huggingface/hub"))
  ```
  The HuggingFace SDK docs state the Windows default is `%USERPROFILE%\.cache\huggingface\hub`. This is mostly right — on Windows `dirs::home_dir()` → `%USERPROFILE%` so the joined path works *if* Windows users keep the HF defaults. However: Windows users who installed Python+transformers got `%LOCALAPPDATA%\huggingface\hub` from older `huggingface_hub` versions, and the conventional Windows cache root has always been `%LOCALAPPDATA%`. `cqs doctor --json` will display a non-existent path on Windows as the "expected HF cache", making the "Model not downloaded" diagnostic misleading.
- **Suggested fix:** Use `dirs::cache_dir()` (which resolves correctly per-OS) joined with `huggingface/hub` as the fallback:
  ```rust
  if let Ok(p) = std::env::var("HF_HOME") { return PathBuf::from(p).join("hub"); }
  if let Ok(p) = std::env::var("HUGGINGFACE_HUB_CACHE") { return PathBuf::from(p); }
  dirs::cache_dir().map(|c| c.join("huggingface/hub"))
      .or_else(|| dirs::home_dir().map(|h| h.join(".cache/huggingface/hub")))
      .unwrap_or_else(|| PathBuf::from(".cache/huggingface/hub"))
  ```
  (still falls through to `~/.cache/huggingface/hub` for Linux/macOS/WSL where that is the documented default.)

#### PB-V1.29-9: `aux_model::expand_tilde` only handles `~/` prefix — misses `~` alone and native Windows `%USERPROFILE%`
- **Difficulty:** easy
- **Location:** `src/aux_model.rs:101-108`
- **Description:**
  ```rust
  fn expand_tilde(raw: &str) -> PathBuf {
      if let Some(stripped) = raw.strip_prefix("~/") {
          if let Some(home) = dirs::home_dir() { return home.join(stripped); }
      }
      PathBuf::from(raw)
  }
  ```
  A user configuring `splade.model_path = "~"` (bare tilde, pointing at home) fails expansion. More importantly, Windows users using `~\Models\splade` (backslash separator) are not expanded, and `cqs-<version>/.cqs.toml` is silently treated as a literal path starting with `~\`. On the `is_path_like` check at line 124, `raw.starts_with("~/")` is also Windows-blind.
- **Suggested fix:** Extend the check to handle `~` alone, `~/`, and `~\` (plus `$HOME` / `%USERPROFILE%` if symmetry is desired):
  ```rust
  fn expand_tilde(raw: &str) -> PathBuf {
      if raw == "~" { return dirs::home_dir().unwrap_or_else(|| PathBuf::from(raw)); }
      if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix(r"~\")) {
          if let Some(home) = dirs::home_dir() { return home.join(rest); }
      }
      PathBuf::from(raw)
  }
  ```
  Apply the same extension to `is_path_like`.

#### PB-V1.29-10: WSL detection via `/proc/version` — misses native Linux containers with "microsoft"/"wsl" in kernel string
- **Difficulty:** medium
- **Location:** `src/config.rs:31-47`
- **Description:**
  ```rust
  pub fn is_wsl() -> bool {
      static IS_WSL: OnceLock<bool> = OnceLock::new();
      *IS_WSL.get_or_init(|| {
          if std::env::var_os("WSL_DISTRO_NAME").is_some() { return true; }
          std::fs::read_to_string("/proc/version")
              .map(|v| v.to_lowercase().contains("microsoft") || v.contains("wsl"))
              .unwrap_or(false)
      })
  }
  ```
  Two issues:
  1. `v.contains("wsl")` is case-sensitive on the second predicate but `.to_lowercase()` was applied only to the first; line 42 stores `.to_lowercase()` in `lower` and checks both, so this particular bug is not active — but the test comparing raw `v` (if refactored) would silently regress.
  2. The detection can false-positive on Linux hosts where `/proc/version` happens to mention Microsoft (e.g. Mariner Linux, some Azure images, or a custom kernel with a "Microsoft" contributor in `CONFIG_CC_VERSION_TEXT`). On those hosts `cqs` then switches to `--poll` mode and bumps debounce to 1500ms for no reason, slowing watch cycles ~3×.
- **Suggested fix:** Also require `WSL_INTEROP` or `/run/WSL` / `/proc/sys/fs/binfmt_misc/WSLInterop` to be present. Those are set exclusively by the WSL init process. Falling back to `/proc/version` alone is the cheapest signal but should not be the only one.

---

## Summary
10 findings: 8 easy + 2 medium. Most are path-normalization gaps where forward-slash DB origins meet backslash or verbatim-prefix user/system input on Windows or WSL. PB-V1.29-2 (silent SPLADE no-op on Windows watch) and PB-V1.29-3 (chunk.id drift on incremental re-index) are the highest-impact correctness issues.

---

## Security

#### DNS-rebinding exfiltration of full source — `cqs serve` accepts any Host header
- **Difficulty:** medium
- **Location:** `src/serve/mod.rs:97-114` (`build_router`), `src/cli/commands/serve.rs:20-34` (`cmd_serve`)
- **Description:** `cqs serve` binds 127.0.0.1 by default but does NOT validate the inbound `Host` header and does NOT send any CORS headers. Classic DNS-rebinding attack: an attacker's web page at `evil.example.com` (TTL 0, returns `A 127.0.0.1` after the user's initial fetch) can `fetch("http://evil.example.com:8080/api/chunk/<id>")` and read the response because the attacker's origin is same-site from the browser's view, while the server sees only a 127.0.0.1 bind and answers. `/api/chunk/:id` returns 30 lines of source (`content_preview` in `data.rs:484`), `/api/search?q=` returns arbitrary name matches, `/api/graph` returns the entire call graph, `/api/embed/2d` returns every chunk with UMAP coords. An adversarial page on another tab can silently exfiltrate the whole indexed corpus. The v1 spec explicitly calls out "single-user local exploration" but the network isolation is paper-thin. PoC: `while true; do curl -s -H 'Host: evil.example.com' http://127.0.0.1:8080/api/stats; done` returns 200 — server answers regardless of Host.
- **Suggested fix:** Add middleware that 400s any request whose `Host:` header isn't `127.0.0.1:<port>`, `localhost:<port>`, `[::1]:<port>`, or the explicit `--bind` value. Optionally tighten with `tower_http::validate_request::ValidateRequestHeaderLayer`. This defeats DNS rebinding because the attacker's DNS record flips the IP, not the host header the browser sends. Same pattern used by `jupyter`, `rust-analyzer`, etc.

#### XSS via unescaped error body in hierarchy view and cluster view
- **Difficulty:** easy
- **Location:** `src/serve/assets/views/hierarchy-3d.js:107` and `src/serve/assets/views/cluster-3d.js:114`
- **Description:** Both views do `this.container.innerHTML = \`<div class="error">... ${body.slice(0, 200)}</div>\`` where `body` is `await resp.text()` from a failed `/api/hierarchy/{id}` or `/api/embed/2d` call. The failure body is JSON but the client injects it as raw HTML. The `{id}` passed to the hierarchy endpoint is reflected unescaped by the server in `ServeError::NotFound(format!("chunk: {id}"))` (`handlers.rs:219`), which lands in `detail` of the JSON body. An attacker who can make a user visit a crafted URL runs arbitrary JS in the cqs-serve origin. PoC: `http://127.0.0.1:8080/?view=hierarchy&root=%3Cimg%20src%3Dx%20onerror%3Dalert(document.domain)%3E` → loadData() fetches `/api/hierarchy/<img src=x onerror=alert(document.domain)>`, gets 404 with `{"error":"not_found","detail":"not found: chunk: <img src=x onerror=alert(document.domain)>"}`, and that string is slotted into innerHTML. `<img>` closes cleanly inside the JSON and the `onerror` fires. Paired with the DNS-rebinding vector above, any site on the internet can run JS against a running `cqs serve` without user interaction beyond "visit the page".
- **Suggested fix:** Either (a) always parse the body as JSON and render `escapeHtml(parsed.detail)` (consistent with the rest of app.js which uses `escapeHtml`), or (b) use `textContent` to set the error message. Easiest: mirror the pattern in `app.js:372` — `<p class="error">error: ${escapeHtml(...)}</p>`.

#### `build_graph` (uncapped) + `build_cluster` fetch entire chunks+function_calls tables with no server-side row limit — memory/DoS
- **Difficulty:** easy
- **Location:** `src/serve/data.rs:232-234, 344-350, 833-861`, `src/serve/handlers.rs:96-119, 227-242`
- **Description:** `/api/graph` with no `max_nodes` param (or `max_nodes=0` — still `Some`) runs `SELECT ... FROM chunks WHERE 1=1` with no LIMIT and pulls the full table into memory (data.rs:232-234), then fetches every row of `function_calls` (data.rs:344-350). `/api/cluster` (`build_cluster`) ignores `max_nodes` during the SQL fetch — it ALWAYS selects every chunk with non-null UMAP coords plus every edge, then truncates only after the full materialized Vec exists (data.rs:833-861, 909-919). On a 16k-chunk corpus with a few hundred thousand call edges that's hundreds of MB of serde_json allocations per request. No rate limiting, no concurrent-request cap, no body/time limit. Combined with the DNS-rebinding vector, a remote attacker can trigger this repeatedly. Even without rebinding, a local adversary can knock the server over by spamming `GET /api/cluster` and `GET /api/graph` with no params.
- **Suggested fix:** (1) Require `max_nodes` (or force a hard default, e.g. min(requested, 10_000)) and push the truncation into SQL via `ORDER BY ... LIMIT`. (2) Add `tower::limit::ConcurrencyLimitLayer` and `tower_http::limit::RequestBodyLimitLayer` on the router. (3) Add `tower_http::timeout::TimeoutLayer` so a runaway query can't pin a worker forever. The graph capped path already does this correctly — apply the same SQL-LIMIT pattern in `build_cluster` and in the uncapped `build_graph` branch.

#### `build_graph` + `build_hierarchy` IN-list can exceed SQLite 32k bind limit → 500 instead of graceful handling
- **Difficulty:** easy
- **Location:** `src/serve/data.rs:326-341` (graph edge fetch), `src/serve/data.rs:670-754` (hierarchy edge fetch)
- **Description:** Both endpoints build `IN (?, ?, ?, ...)` dynamically from user-triggered BFS or node-set size and then bind every element twice. `build_graph`: binds = 2 × unique_names (edge SQL). `build_hierarchy`: binds = 2 × visited_names (edge SQL). SQLite's default `SQLITE_LIMIT_VARIABLE_NUMBER` is 32766; on a large corpus with `max_nodes=16001` or a deep/wide BFS expansion on a densely-connected function like `unwrap`, the bind count exceeds the limit and sqlx returns an error. The current code propagates the error to the client as `ServeError::Store` → 500. An attacker (or an agent passing `max_nodes=32000` because they didn't read the frontend cap) can trip this. On indexes with heavily-overloaded names (e.g. many `new` methods), it's trivially reachable.
- **Suggested fix:** Chunk the IN-list across multiple queries and union the results in Rust, OR cap `max_nodes` in `handlers::graph` before it reaches `build_graph` (e.g. clamp to 10_000). For hierarchy, enforce a post-BFS `visited_names.truncate(MAX_HIERARCHY_NODES)` before building the edge IN-list. Same pattern used by `tests/eval_common.rs::batch_in_chunks` for deletes.

#### `GraphQuery.file` filter passes raw user input through SQL `LIKE` — wildcard injection
- **Difficulty:** easy
- **Location:** `src/serve/data.rs:241-248`
- **Description:** `build_graph` binds `format!("{file}%")` against SQL `LIKE ?`. The parameter is properly bound so it's not SQL injection, but `%` and `_` are LIKE metacharacters. A request with `?file=%sensitive%` matches anywhere the string "sensitive" occurs — not the prefix the API contract advertises. Similarly, `?file=_` matches any single-char file prefix. Not a high-severity vulnerability by itself (attacker only enumerates files they could discover via `/api/graph` anyway), but it breaks the API contract and may interact badly with future filters that trust the `file` parameter as a "safe prefix" — e.g. a future CSV export that uses the same filter to scope the response. The docstring says "file-path filter" implying a prefix; the behavior is "contains".
- **Suggested fix:** Escape `%`, `_`, and the ESCAPE-character in the user-supplied `file` before interpolating: `let escaped = file.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"); binds.push(format!("{escaped}%"));` and add `ESCAPE '\\'` to the SQL: `AND c.origin LIKE ? ESCAPE '\\'`.

#### `cmd_serve` spawns `xdg-open`/`open`/`explorer.exe` on a URL containing a user-supplied bind string — command-string injection surface
- **Difficulty:** hard
- **Location:** `src/cli/commands/serve.rs:50-78`
- **Description:** When `--open` is passed, `cmd_serve` builds `format!("http://{bind_addr}")` and invokes `std::process::Command::new(cmd).arg(url).spawn()`. `bind_addr` is the parsed `SocketAddr`, so direct shell metacharacters are rejected at parse time — but the URL still flows into `xdg-open`, which on Linux chains through `exo-open`/`gio-open`/the user's `xdg-mime` handler, and those downstream handlers have their own quirks (e.g. MIME handlers that forward the URL to a browser that does re-parsing). More concerning: `--bind` accepts `localhost` and `::1` as strings, and `cqs serve --bind localhost:evil --open` would produce `http://localhost:evil:<port>/` — the actual bind parse would fail first because `localhost:evil` isn't a valid socket address, but future refactors that accept hostname-style binds (e.g. accepting IPv6 with zone identifiers like `fe80::1%eth0`) could produce URLs with `%` / `/` characters that confuse downstream handlers. The zone-id case (`%eth0`) is particularly interesting because `%` is valid URL-encoded syntax.
- **Suggested fix:** URL-encode or strictly allowlist the bind string before inserting it into the URL. Better: only ever open `http://127.0.0.1:<port>` or `http://[::1]:<port>` regardless of the actual bind (if `--bind 0.0.0.0` is passed with `--open`, the user still wants to visit their local browser pointing at localhost, not 0.0.0.0).

#### `cqs serve` has no authentication — default stance relies entirely on "localhost is trusted"
- **Difficulty:** hard
- **Location:** `src/serve/mod.rs:97-114`, `src/cli/commands/serve.rs:20-30`
- **Description:** There is no token, cookie, or per-session check on any endpoint. The `--bind 0.0.0.0` warning at `src/cli/commands/serve.rs:24-29` is cosmetic — it prints to stderr but still starts the server open to the network. Agents / operators who see the warning in a log tail later have no runtime defense; the server keeps serving unauthenticated requests forever. Every other local-first dev server that's even vaguely exposed (VS Code live preview, Jupyter, rust-analyzer) at least binds a per-process random token. Also: no TLS, so even the localhost-only posture is vulnerable to any user on the same box (multi-user workstations, shared CI runners, ill-gotten local container escapes) reading the entire index including source content via `/api/chunk/:id`.
- **Suggested fix:** Generate a per-launch random token in `cmd_serve`, print it in the "listening on ..." banner as `http://127.0.0.1:8080/?token=<hex>`, require it on every `/api/*` request (either query param or `X-Cqs-Token` header). Hashed-compare so a timing attack can't recover bytes. Optionally short-lived session cookie set by the token-carrying first request.

#### LIKE injection in `build_chunk_detail` "tests-that-cover" heuristic — user-controlled name used in LIKE without escaping
- **Difficulty:** easy
- **Location:** `src/serve/data.rs:533-541`
- **Description:** The tests heuristic binds `format!("%{name}%")` where `name` is the chunk's stored name from the database. Stored names originate from parsed source — an adversary who indexed their hostile project into cqs (e.g., via `cqs ref add` pointing at attacker-controlled code) can create a function named something like `_%` or `%_%`. When a downstream user clicks that chunk in the UI, the server executes `content LIKE '%_%%'` which matches far more rows than intended, returning up to 20 unrelated "test" chunks. That's a denial-of-accuracy: the UI's "tests that cover" panel is populated with junk. On a very large index, a name of `_%` would match essentially every test chunk, forcing a full table scan and slowing the endpoint. Paired with concurrent requests (see DoS finding) an attacker could grind the daemon down.
- **Suggested fix:** Escape LIKE metacharacters in `name` the same way as the `file_filter` fix above (`% → \%`, `_ → \_`), append `ESCAPE '\\'`. Better: switch the heuristic to FTS5 (exact token match against `chunks_fts` like `search_by_name` already does) — the current LIKE-based heuristic is O(n) table scan anyway.

## One-line summary

Found eight security issues, all in the new `cqs serve` surface: DNS-rebinding exfiltration (no Host check + no CORS), two reflected-XSS paths via `innerHTML = body.slice()` in hierarchy-3d and cluster-3d views, two DoS-class issues (unbounded SQL + IN-list bind overflow), one LIKE-wildcard injection in the `file` graph filter, one LIKE injection in the chunk-detail "tests" heuristic, plus unauthenticated serve and an `xdg-open` URL-interpolation hazard.

---

## Data Safety

#### DS2-1: `prune_missing` reads origin list OUTSIDE write transaction — same TOCTOU that P2 #32 closed for `prune_all`
- **Difficulty:** easy
- **Location:** `src/store/chunks/staleness.rs:76-90`
- **Description:** The previous audit closed the Phase 1 / Phase 2 race in `prune_all` by moving the `SELECT DISTINCT origin FROM chunks` inside the `begin_write()` transaction. `prune_missing` still does the reverse: it fetches `rows` from `&self.pool` at line 76, computes `missing` from that snapshot, and only *then* calls `begin_write()` at line 102. During the gap `cqs watch` (which only holds `try_acquire_index_lock`, not the DB `WRITE_LOCK`) can call `upsert_chunks_and_calls` for a freshly-added file. That file is missing from the caller-supplied `existing_files` snapshot but present in `chunks`; the Phase 2 DELETE will wipe the just-added rows. `prune_missing` is called by both the daemon startup GC (`cli/watch.rs:944`) and the periodic GC (`cli/watch.rs:1026`), so the window is reachable every time either fires.
- **Suggested fix:** Move the `SELECT DISTINCT origin FROM chunks` call inside the `begin_write()` transaction (swap the order at line 75 and 102). Mirror the `prune_all` layout exactly — take the write lock first, then fetch distinct origins from `&mut *tx` so the DELETE operates on the same snapshot the read observed.

#### DS2-2: `prune_gitignored` reads origin list OUTSIDE write transaction — same class as DS2-1
- **Difficulty:** easy
- **Location:** `src/store/chunks/staleness.rs:333-337`
- **Description:** `prune_gitignored` (called on every daemon startup via `run_daemon_startup_gc` and every 30 min via `run_daemon_periodic_gc`) runs `SELECT DISTINCT origin FROM chunks` on `&self.pool` outside a transaction (line 336), then later opens `begin_write` at line 374 and does batched DELETEs. If `cqs watch`'s reindex commits between the two phases for a newly-gitignored path (rare but possible via staged .gitignore edits), the delete set reflects the stale Phase-1 snapshot and a just-written chunk survives the prune — or, more consequentially, a chunk whose matcher result changed in the gap gets deleted even though its `chunks` row was refreshed in the interim. The function comment at line 331 even acknowledges "Rust-side filter, outside tx so the matcher walk doesn't hold the write lock" as if the hazard were a deliberate trade — but the matcher walk is microseconds on ~10k origins, not a meaningful lock hold.
- **Suggested fix:** Open the `begin_write` transaction first and issue the `SELECT DISTINCT origin` against `&mut *tx` (as `prune_all` now does). The matcher walk then proceeds inside the tx — trivial cost compared to the DELETE phase that already runs there.

#### DS2-3: `set_metadata_opt` and `touch_updated_at` bypass `WRITE_LOCK` — torn-write race against `begin_write()` transactions
- **Difficulty:** easy
- **Location:** `src/store/metadata.rs:409-418` (`touch_updated_at`), `:452-475` (`set_metadata_opt`)
- **Description:** `set_hnsw_dirty` at line 436-449 correctly uses `begin_write()` (added by DS-V1.25-3) to serialize against every other in-process writer. The sibling functions `touch_updated_at` and `set_metadata_opt` still execute raw pool writes: `sqlx::query(...).execute(&self.pool)` with no `WRITE_LOCK` guard. Two in-process writers — e.g. the daemon's watch loop calling `touch_updated_at()` at `cli/watch.rs:2590` and the pipeline path simultaneously running `upsert_chunks_and_calls` — can both have deferred transactions open, and SQLite returns `SQLITE_BUSY` when one of them tries to upgrade to exclusive. `WRITE_LOCK` was added specifically to prevent this — `set_metadata_opt` is called by pending-batch ID setters (`set_pending_batch_id`, `set_pending_doc_batch_id`, `set_pending_hyde_batch_id`) used by long-running LLM batch polling, which are exactly the paths where a concurrent reindex is likely.
- **Suggested fix:** Rewrap both in `let (_guard, mut tx) = self.begin_write().await?` + execute against `&mut *tx` + `tx.commit()`. Same pattern as `set_hnsw_dirty`. The write is a one-statement UPSERT/DELETE, so the transactional overhead is negligible, and the invariant "every in-process mutation takes WRITE_LOCK" becomes structural instead of "call the right setter."

#### DS2-4: Phantom-chunks DELETE is in a separate transaction from the chunk upsert that precedes it — mid-batch crash leaves the index inconsistent with disk
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:2568-2579`
- **Description:** For each file in the watch cycle, the loop runs `store.upsert_chunks_and_calls(pairs, ...)?` followed by `store.delete_phantom_chunks(file, &live_ids)?` — each in its own independent write transaction. The source code comment at line 2574-2577 explicitly acknowledges "Ideally this would share a transaction with `upsert_chunks_and_calls`, but both methods manage their own internal transactions. A crash between the two leaves phantoms that get cleaned on the next reindex." This is hand-waved as tolerable because the next reindex will clean it, but there's a worse class of failure mode: if the daemon crashes between the upsert (which marks `hnsw_dirty=1` at `watch.rs:2179`) and any subsequent HNSW rebuild — a long window on large batches — the HNSW has been flagged dirty but phantoms still exist and will match search queries against IDs that are about to be deleted. The `hnsw_dirty` flag is only cleared after a full rebuild following the final file; if the process is SIGKILLed mid-batch, we serve queries against a half-pruned index for the next daemon boot until the first event triggers a rebuild.
- **Suggested fix:** Extend `upsert_chunks_and_calls` to accept a `live_ids: Option<&[&str]>` argument that, when present, performs the phantom delete inside the same tx — mirroring how `prune_all` does four logical operations in one transaction. Alternative: add a single `upsert_file_chunks(file, chunks_with_embeddings, calls, live_ids)` that wraps the whole per-file delta in one BEGIN/COMMIT, and let `cli/watch.rs:2538-2580` call it per file instead of the two-step dance.

#### DS2-5: `EmbeddingCache::evict` is TOCTOU — SELECT size / AVG runs on pool, DELETE runs on pool — concurrent writers invalidate the eviction decision
- **Difficulty:** easy
- **Location:** `src/cache.rs:352-400`
- **Description:** `evict()` queries `SELECT SUM(LENGTH) + COUNT*200 FROM embedding_cache`, then `SELECT AVG(LENGTH + 200)`, then `DELETE WHERE rowid IN (SELECT ... LIMIT ?)` — all three on `&self.pool`, each in its own implicit transaction. Between the size measurement and the DELETE, a concurrent `write_batch` (which uses `pool.begin()` with no shared lock; the cache has its own pool and no cross-call WRITE_LOCK equivalent) could add 10+ MB of entries; the computed `entries_to_delete` is then wrong, and we evict based on a stale snapshot. Under the daemon's periodic-evict-while-writing pattern this is the hot path: evict runs alongside `write_batch` in the same tokio runtime. Worse, two `evict()` calls from different threads can both decide to delete from the same `LIMIT ?1` prefix — the `ORDER BY created_at ASC` makes the deletes overlap and each caller's `rows_affected()` count is larger than the per-call contribution. `QueryCache::evict` at `:1103-1147` has the identical shape and the identical bug.
- **Suggested fix:** Wrap all three SELECTs + the DELETE in a single `pool.begin()` transaction so the size measurement, AVG computation, and DELETE see one consistent snapshot. Add an in-process `Mutex<()>` guard (either on the `EmbeddingCache` struct or a process-global like SQLite's `WRITE_LOCK`) so two evicts can't race at all. Since `evict()` is idempotent to some extent, this is low-risk — but the mis-accounting currently shows up as "evicted more than needed" ratchets during parallel reindex loops.

#### DS2-6: HNSW save's `.bak` rename sequence writes `.bak` without fsync before `atomic_replace` — power cut between step-3 rename and step-4 atomic_replace can lose the directory entry
- **Difficulty:** medium
- **Location:** `src/hnsw/persist.rs:414-426` (backup rename) + `:429-461` (atomic_replace)
- **Description:** Save sequence: (1) build temp_dir with new files + checksums; (2) acquire lock; (3) for each ext, `std::fs::rename(final, bak)` at line 418 — plain `rename`, no fsync of the parent dir after; (4) for each ext, `atomic_replace(temp, final)` which does fsync the parent. Between step 3 and step 4, the parent directory's rename-to-`.bak` entries are NOT fsynced. On a power cut with ext4's default `data=ordered` we're likely fine because journal ordering commits the rename entry before subsequent writes — but the exact guarantee depends on the filesystem. If step 4 fails after step 3 completed, rollback path at line 465-487 tries `std::fs::rename(bak, final)` to restore — again no fsync on the restore rename, and no fsync of the parent dir after the rollback completes. A second power cut during rollback can leave the index with missing files even though the .bak existed on disk. The intermediate window is small but real: a multi-file rollback (graph + data + ids + checksum) takes multiple rename syscalls.
- **Suggested fix:** After the back-up loop at line 418-425 but before the atomic_replace loop, open the parent `dir` and call `sync_all()` once. Same at the end of the rollback restore loop at line 471-483. A single fsync amortized across 4 renames is negligible overhead compared to the HNSW save's cost (which is dominated by graph+data writes, hundreds of MB). The `atomic_replace` helper already does this for its own pass; the backup pre-pass just needs the same treatment.

#### DS2-7: HNSW incremental insert + save is not atomic against the `hnsw_dirty` metadata flag — crash leaves `dirty=1` permanently even though the on-disk file was fully written
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:2326-2338` (incremental save + clear-dirty), `:2250-2257` (full rebuild + clear-dirty)
- **Description:** Both the incremental and the full-rebuild branches clear `hnsw_dirty` only AFTER `index.save(...)` succeeds. If the save itself succeeds but `set_hnsw_dirty(Enriched, false)` fails (transient SQLITE_BUSY, daemon read-only mode racing with a manual `index --force`, etc.) we log a warning and continue. Now the on-disk HNSW is actually fresh but the metadata says "dirty," so every subsequent reopen of the Store at `src/cli/store.rs:289` will see `is_hnsw_dirty() == true` and rebuild from scratch — paying 10s-1m of full-graph-rebuild cost per daemon restart. More importantly, in the reverse ordering (`watch.rs:2179-2186`) the flag is set to `dirty=1` before the chunks write, then the write happens, then we try to clear. If the process is SIGKILLed between the chunks commit and the HNSW save, the next daemon load sees dirty + stale HNSW and correctly rebuilds — that path is safe. It's the "save succeeded but clear-dirty failed" path that is unrecoverable without a manual reindex.
- **Suggested fix:** The clear-dirty is a single metadata UPDATE; if it returns an error, retry once (with `begin_write` retry semantics) before giving up. Alternatively, expose `set_hnsw_dirty_after_save(...)` that reads the on-disk checksum and only reports "dirty" if checksum mismatches actual file — moving the invariant from metadata-must-agree-with-filesystem to filesystem-is-source-of-truth. Simpler fix: tie the clear-dirty to a blake3 of the saved files; on reopen, if the blake3 matches the stored value, treat as clean regardless of the dirty flag. That turns the dirty flag into an optimization rather than a correctness requirement.

#### DS2-8: `v18→v19` migration drops orphan sparse_vectors rows WITHOUT updating schema_version in the same transaction — rollback on error leaves half-migrated state (prior to P1 #16 UPSERT fix)
- **Difficulty:** medium (but may already be mitigated by backup-restore path)
- **Location:** `src/store/migrations.rs:478-562` (`migrate_v18_to_v19`) + `:197-203` (UPSERT of schema_version)
- **Description:** The `migrate_v18_to_v19` function does (in order inside the tx): CREATE sparse_vectors_v19, INSERT...SELECT from sparse_vectors INNER JOIN chunks (drops orphans), DROP INDEX idx_sparse_token, DROP TABLE sparse_vectors, ALTER TABLE RENAME to sparse_vectors, CREATE INDEX idx_sparse_token, bump splade_generation. Then at `run_migration_tx` line 197-203 the caller UPSERTs schema_version. These are all in one tx, so SQLite rollback-on-error is safe per-SQLite. But the v18→v19 migration is destructive: it PERMANENTLY drops orphan sparse_vectors rows as part of the copy (the INNER JOIN filter at line 508). If the migration fails AFTER the INSERT step (say, during the ALTER TABLE RENAME or the CREATE INDEX), the rollback correctly reverts everything — but the filesystem backup at `src/store/backup.rs:107` taken before the migration is the only recovery path. If that backup step itself silently failed (because `CQS_MIGRATE_REQUIRE_BACKUP` is not set, which is the default), the log says "Migration backup failed; proceeding without snapshot" and then the migration proceeds. A subsequent non-transactional error — e.g. a commit-time IO failure that SQLite can't fully reverse — leaves the user with a partially-migrated DB and no backup to restore from. The happy-path case is fine; the no-backup + non-transactional-commit-failure case is catastrophic.
- **Suggested fix:** Flip the default: `CQS_MIGRATE_REQUIRE_BACKUP` should default to `1`, not `0`. The current default ("proceed without snapshot") is the correct choice only for users on filesystem-full scenarios; for everyone else, a backup failure is a signal to abort before doing anything destructive. Alternative: add a pre-migration `SELECT COUNT(*) FROM sparse_vectors` vs `SELECT COUNT(*) FROM sparse_vectors_v19` assertion that refuses to DROP the old table if the INSERT filter removed rows we weren't expecting to lose. Log a `tracing::error!` and require `CQS_ALLOW_ORPHAN_DROP=1` env to proceed.

#### DS2-9: `upsert_sparse_vectors` drops the secondary `idx_sparse_token` index INSIDE the write tx — a panic after the DROP but before the CREATE leaves the index without the token_id B-tree, killing SPLADE search on reopen
- **Difficulty:** medium
- **Location:** `src/store/sparse.rs:113-117` (DROP) + `:193-196` (CREATE INDEX at the end)
- **Description:** The bulk-load pattern DROP INDEX → INSERT in batches → CREATE INDEX is standard SQLite optimization, and because it's all inside one `begin_write()` transaction, SQLite's rollback semantics correctly revert both the DROP and any INSERT on error. HOWEVER: `sqlx::Transaction` holds an `&mut SqliteConnection`. The inner loop at line 162-177 does `qb.build().execute(&mut *tx).await?` — a network or I/O error on any batch returns early, the `?` propagates, and `tx` is dropped implicitly which triggers a rollback ONLY if the connection is still alive. Under panic unwind (e.g. a downstream `split_at_unchecked` OOB panic when weight is NaN), the rollback also fires via `Drop`. What does NOT fire a rollback: if `upsert_sparse_vectors` itself is canceled asynchronously (e.g. the tokio runtime dropped by CTRL-C while this function is mid-transaction under `rt.block_on`). `block_on` is synchronous, so `Drop::drop(tx)` runs, and SQLite should roll back — so this is actually safe in principle. But the failure mode I'm worried about: if the process is SIGKILLed while the tx is open, SQLite's WAL recovery on next open will see the uncommitted INSERTs as unreachable in the WAL and discard them — the DDL (DROP INDEX / CREATE INDEX) that SQLite treats as a schema-altering operation likewise rolls back via WAL. Net: safe under kill-9. The real concern is more subtle: if an admin `kill -9`s the process after the transaction committed BUT BEFORE the CREATE INDEX statement fired (impossible in practice because they're both in the same tx, but I'm exhaustively exercising the concern) — OK, this case cannot happen. The legitimate residual issue: if the bulk insert fails and returns early, the transaction rolls back, but `bump_splade_generation_tx` at line 202 was also rolled back. Net: no generation bump means the on-disk `splade.index.bin` from a prior run is still trusted, while `sparse_vectors` rows were never added. Readers see stale SPLADE results until the next successful upsert bumps the counter.
- **Suggested fix:** On `upsert_sparse_vectors` error, call `bump_splade_generation` (a standalone function, NOT inside any tx) to force invalidation of the on-disk file that could now be out-of-step with a rollback. This converts "rollback + stale on-disk file" into "rollback + regenerated on-disk file," which is the correct invalidation direction. Add the cleanup to the error path of the function itself, outside `self.rt.block_on(async { ... })`.

#### DS2-10: `as_millis() as i64` mtime cast silently truncates — if a user's system clock jumps ~292 million years forward or if a pathological filesystem returns mtime beyond `i64::MAX` milliseconds, comparisons against stored_mtime wrap silently and flag fresh files as stale
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:2556`, `src/lib.rs:454`, `src/store/chunks/staleness.rs:511,601,674,834,879,1332,1395`, `src/store/notes.rs:150,354`
- **Description:** `Duration::as_millis()` returns `u128`. The cast `as i64` silently wraps when the value exceeds `i64::MAX` (year 2262-ish) — for realistic wall clock this is never a concern. The real concern is pathological mtime values: a filesystem bug (ZFS snapshot with a bad atime, a clock-jumped VM that briefly set mtime in the far future, WSL 9P's notorious mtime clamping) can return mtimes that survive `duration_since(UNIX_EPOCH)` as a positive `Duration` but overflow `i64::MAX` on the cast. The store compares these as `i64` in `needs_reindex`, `list_stale_files`, `check_origins_stale`, `notes_need_reindex`. A silently-wrapped negative `i64` compares as LESS than any legitimate stored_mtime, which means: (a) `needs_reindex` returns `Some(wrapped_neg_mtime)` because `stored_mtime >= current` is false, triggering a wasteful reindex — benign but noisy; (b) `list_stale_files` puts the file in `stale` because `current > stored` — also benign; (c) `notes_need_reindex` matches the else arm and returns a wrapped `current_mtime` that gets WRITTEN into the `notes.file_mtime` column as a negative i64, permanently polluting the DB until manual cleanup.
- **Suggested fix:** Replace the 13 sites with a shared helper `fn duration_to_mtime_millis(d: Duration) -> i64 { i64::try_from(d.as_millis()).unwrap_or(i64::MAX) }`. Saturating semantics match the "future time is still fresh" invariant, and the one-site-changes-all-callers pattern prevents drift. Defensive but cheap; also makes the cast intent explicit.

Summary: 10 actionable findings covering schema migration risk (DS2-8), TOCTOU in GC paths (DS2-1, DS2-2), torn writes from WRITE_LOCK bypass (DS2-3), atomicity gaps across multi-step watch reindex (DS2-4), cache evict race (DS2-5), HNSW parent-dir fsync gaps (DS2-6), HNSW/SQLite dirty-flag state drift (DS2-7), SPLADE generation bump rollback (DS2-9), and mtime cast truncation (DS2-10). The SPLADE/HNSW/chunks atomicity invariant is strong where the prior audits fixed it (`prune_all`, `set_hnsw_dirty`, `upsert_sparse_vectors`) but the fixes didn't propagate uniformly — two sibling prune functions and two metadata setters still run outside the `WRITE_LOCK` discipline.

---

## Performance

#### [PF-V1.29-1]: Daemon request path shell-joins and re-splits args on every query
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:315-331`
- **Description:** For every daemon socket query, `handle_socket_client` extracts `command: String` and `args: Vec<String>` from the JSON request (`src/cli/watch.rs:229-270`), then reconstructs a single string via `format!("{} {}", command, shell_words::join(&args))` and passes it to `BatchContext::dispatch_line`, which immediately re-splits it with `shell_words::split` (`src/cli/batch/mod.rs:563`). This is a pure waste on the hot daemon path — every query pays: (1) `shell_words::join` (quoting + escape pass, allocates per arg), (2) `format!` allocation of the assembled line, (3) `shell_words::split` on the daemon side (another allocation + tokenization pass), (4) both paths validate NUL bytes on the same data. For agents firing 100+ queries per task, this is hundreds of redundant String allocations and two full passes over the tokens. The Vec<String> that arrives already has the shape `BatchInput::try_parse_from(&tokens)` expects.
- **Suggested fix:** Add a `dispatch_tokens(&self, tokens: &[String], out: &mut impl Write)` method on `BatchContext` that takes already-parsed tokens directly. `handle_socket_client` prepends `command` to `args` (or uses `std::iter::once(command).chain(args.iter())`) and calls `dispatch_tokens`. `dispatch_line` can keep its shell parsing path for the `cqs batch` stdin surface but skips the round-trip for daemon queries. Also eliminates one of two `reject_null_tokens` checks since the JSON parser's string validation already covers NUL bytes on the socket path.

#### [PF-V1.29-2]: `fetch_chunks_by_ids_async` and `fetch_candidates_by_ids_async` hardcode BATCH_SIZE=500, ignore modern SQLite limit
- **Difficulty:** easy
- **Location:** `src/store/chunks/async_helpers.rs:27, 69` (both functions)
- **Description:** Both fetch helpers hardcode `const BATCH_SIZE: usize = 500` with comments claiming "SQLite's 999-parameter limit". That limit was raised to 32766 in SQLite 3.32 (2020). The rest of the codebase (`src/store/calls/query.rs:204`, `src/store/types.rs:18`, `src/store/sparse.rs:123`, `src/store/calls/crud.rs:32,81,115,217,283,313`) already uses `crate::store::helpers::sql::max_rows_per_statement(1)` which returns ~32466. These are called from every search: `search_by_candidate_ids_with_notes` → `fetch_candidates_by_ids_async` (line 860 of search/query.rs) and `finalize_results` → `fetch_chunks_by_ids_async` (line 412 of search/query.rs). On wide queries (e.g. `cqs search "X" --limit 100 --rerank` which pools to `limit * 3 = 300`), each call fits in one statement anyway — but the `cqs context` batch fetch (same helper) routinely hits 1000+ IDs and pays 2-3× the round trips.
- **Suggested fix:** Replace the two hardcoded `BATCH_SIZE = 500` with `max_rows_per_statement(1)`. Drop the stale "999-param limit" comment. Same one-line change the other modules already made.

#### [PF-V1.29-3]: `get_type_users_batch` and `get_types_used_by_batch` hardcode BATCH_SIZE=200 — impact analysis pays 3× latency
- **Difficulty:** easy
- **Location:** `src/store/types.rs:392, 438`
- **Description:** Both batch type-edge queries declare `const BATCH_SIZE: usize = 200`. On `cqs impact` for a function that uses 500+ types (common for Rust code — every `HashMap`, `Vec`, `Result`, custom struct counts), `find_type_impacted` at `src/impact/analysis.rs:450` drives 3+ SQL round trips per impact call when one would suffice. Each SQL JOIN (type_edges→chunks) also reloads the full chunk row. Adjacent batch functions in the same file switched to `max_rows_per_statement()` three versions ago; these two slipped through.
- **Suggested fix:** Replace both constants with `max_rows_per_statement(1)` (one bind per row). Already imported at `src/store/types.rs:18`. Single-line change per function.

#### [PF-V1.29-4]: `find_hotspots` allocates String for every callee in the graph before truncating
- **Difficulty:** easy
- **Location:** `src/impact/hints.rs:261-271`
- **Description:** `find_hotspots(graph, top_n)` iterates `graph.reverse.iter()`, calls `name.to_string()` on every entry to build a `Hotspot { name, caller_count }`, sorts the full Vec, then truncates to `top_n`. `graph.reverse` keys are `Arc<str>` (`src/store/calls/query.rs:113-117`). On a 15k-chunk codebase with ~5k distinct callees (reality per `cqs health --json`: 1838 for `assert`, 1771 for `assert_eq`, etc.), the function allocates 5k Strings for every call even though callers want `top_n = 5` (health) or `top_n = 20` (suggest). Pattern is O(n) allocations + O(n log n) sort when a bounded-heap + conditional allocation would be O(n log top_n) and `top_n` allocations.
- **Suggested fix:** Use a `BinaryHeap<(Reverse<usize>, Arc<str>)>` capped at `top_n`, pushing `(reverse.len(), Arc::clone(name))` (Arc clone is refcount bump, not alloc). Drain into `Vec<Hotspot>` at the end with exactly `top_n` `name.to_string()` calls. Cuts allocator churn on the health/suggest hot paths by ~250× for a 5k-callee graph with top_n=20.

#### [PF-V1.29-5]: Parser reads every source file, then unconditionally allocates a full CRLF-replaced copy
- **Difficulty:** easy
- **Location:** `src/parser/mod.rs:491`
- **Description:** `let source = source.replace("\r\n", "\n");` runs for every parsed file regardless of platform or actual CRLF presence. `String::replace` always allocates a fresh String the size of the input. On Linux (the primary development/CI platform) 99%+ of files have no CRLF, yet every parse pays a full-content allocation + memcpy. For the cqs codebase that's 607 files ranging up to 100KB+; on a fresh `cqs index` that's ~50MB of wasted allocations plus the I/O pressure from zeroing the new buffers. `source.contains("\r\n")` is a single linear scan with no allocation — cheap to check before allocating.
- **Suggested fix:** Guard the replace: `let source = if source.contains("\r\n") { source.replace("\r\n", "\n") } else { source };` Preserves CRLF-normalization semantics for actual CRLF files (Windows-authored docs, some config formats) while eliminating the alloc on the common case. Alternatively, use `memchr`-based scan for the `\r` byte only.

#### [PF-V1.29-6]: `BatchContext::notes()` clones the full notes Vec on every cache hit
- **Difficulty:** medium
- **Location:** `src/cli/batch/mod.rs:1015-1064`
- **Description:** `notes()` returns `Vec<cqs::note::Note>` and unconditionally clones the cached Vec on every call (`cached.as_ref()?.clone()` at line 1021 and `result = notes.clone()` at line 1061). For 202 notes (per `cqs health` in this repo), each call clones 202 `Note` structs — each carries `text: String`, `mentions: Vec<String>`, and other owned fields. Callers at `src/cli/batch/handlers/misc.rs:92` (scout), `src/cli/batch/handlers/info.rs:365, 400` (notes list, warnings) only need read access. Compare to sibling `test_chunks()` (line 1101) and `call_graph()` (line 1083) which correctly return `Arc<...>` for cheap O(1) clone. The inline comment at line 1004-1006 about cheap `AuditMode` cloning is correct for audit state but `notes()` is pasted-in and structurally different.
- **Suggested fix:** Change the cache type from `RefCell<Option<Vec<Note>>>` to `RefCell<Option<Arc<Vec<Note>>>>`. Return `Arc<Vec<Note>>`. Update three call sites (`misc.rs:92`, `info.rs:365, 400`) to match — they currently `&notes` and iterate, trivial change. Saves 202 String allocations × 3 call sites per batch query that touches notes.

#### [PF-V1.29-7]: `notes.rs::upsert_notes_batch` runs 3 SQL statements per note in a loop
- **Difficulty:** medium
- **Location:** `src/store/notes.rs:76-87` and the inner `insert_note_with_fts` at `src/store/notes.rs:30-58`
- **Description:** `upsert_notes_batch` loops over notes, calling `insert_note_with_fts` for each. That helper runs 3 statements: INSERT OR REPLACE into `notes` + DELETE from `notes_fts` + INSERT into `notes_fts`. For 200 notes, that's 600 prepared-statement round trips within the transaction. Unlike `upsert_chunks_batch` (which batches into multi-row INSERTs at `src/store/chunks/crud.rs:214`), notes use the per-row path. `replace_notes_for_file` at line 124-128 has the same pattern. Notes are smaller than chunks but the watch loop reindexes notes on every notes.toml edit — with 200+ notes and active note editing during audit sessions, this is ~3000× the round-trip overhead of a batched insert.
- **Suggested fix:** Follow the `upsert_chunks_batch` pattern — build a `QueryBuilder` that emits `INSERT OR REPLACE INTO notes (...) VALUES (?,?,?), (?,?,?), ...` chunked at `max_rows_per_statement(N)` rows per statement. FTS5 unfortunately doesn't support multi-row INSERT via `QueryBuilder::push_values` as cleanly (FTS5 has virtual-table quirks), but batching the DELETE `WHERE id IN (?,?,?...)` collapses N DELETEs into one, leaving only the per-row INSERT INTO notes_fts.

#### [PF-V1.29-8]: `prune_missing` fires `dunce::canonicalize` syscall per missing-path candidate
- **Difficulty:** medium
- **Location:** `src/store/chunks/staleness.rs:27-47` (`origin_exists`) called from `src/store/chunks/staleness.rs:88`
- **Description:** `prune_missing` enumerates all distinct file origins in the chunks table (often 10k+ on real-world projects), then for each one calls `origin_exists(origin, existing_files, root)`. That function first does a HashSet lookup; on miss it falls through to `dunce::canonicalize(&absolute)`, which is a real filesystem syscall per candidate. On the watch hot path with incremental reindex, this fires every reindex cycle; on the initial `cqs index` it fires for every origin in the DB. If `existing_files` was built with canonicalized paths and chunk origins are stored relative (the common case), *every* origin takes the canonicalize fallback. For 15k chunks and 607 distinct origins (per cqs health) that's 607 extra syscalls per prune. WSL filesystem canonicalize over NTFS mount is notoriously slow (~100µs per call) so this can be 60ms per prune on top of the actual delete cost.
- **Suggested fix:** Either: (1) normalize `existing_files` to also contain the relative form at build time so the cheap HashSet path always hits; or (2) build a second HashSet of origins that appear in chunks and subtract from `existing_files` via set difference (O(n+m) instead of O(n×syscall)). Or (3) canonicalize origins once at index time and store the canonical form so staleness is a pure HashSet lookup. Option 3 is the cleanest but requires schema touch; option 1 is zero-schema and resolves the WSL hot spot.

#### [PF-V1.29-9]: `suggest_tests` calls `reverse_bfs` inside a loop over callers — O(callers × graph_size)
- **Difficulty:** hard
- **Location:** `src/impact/analysis.rs:320-335`
- **Description:** For every caller in `impact.callers`, `suggest_tests` runs a fresh `reverse_bfs(&graph, &caller.name, DEFAULT_MAX_TEST_SEARCH_DEPTH)` to determine if that caller is reached by any test. The inline comment at line 322-327 acknowledges the concern but justifies it as "caller count is typically small". On a function with 50+ direct callers (typical for utility functions in a 15k-chunk codebase — `find_hotspots` output shows some functions with 1800+ callers), this is 50 graph traversals, each potentially visiting thousands of ancestor nodes up to depth 5. Degrades with codebase size and test-graph connectivity. The comment claims `reverse_bfs_multi_attributed` can't replace it because it attributes to only one source, but a single forward `bfs_from_tests` (starting at test nodes, walking to targets up to MAX_TEST_SEARCH_DEPTH) computes "is X reached by any test?" for every X in one pass.
- **Suggested fix:** Replace the per-caller BFS with a single pre-computed `reachable_from_tests: HashSet<&str>` — do one forward BFS from each test chunk up to depth N, union the reached sets. Then `is_tested = reachable_from_tests.contains(&caller.name)` is O(1). Reuses the same `graph.forward` adjacency. Cuts `cqs impact --suggest-tests` latency from O(callers × graph) to O(tests + callers). Even on small codebases the computation amortizes; for the cqs self-check with 3531 test chunks, the savings are substantial.

#### [PF-V1.29-10]: `search/query.rs` finalize_results clones sanitized FTS string for no reason
- **Difficulty:** easy
- **Location:** `src/search/query.rs:363-369`
- **Description:** In `finalize_results`:
```rust
let sanitized = sanitize_fts_query(&normalized);
let expanded = expand_query_for_fts(&sanitized);
let fts_query = if expanded.is_empty() {
    sanitized.clone()    // <-- unnecessary clone
} else {
    expanded
};
```
`sanitized` is owned and not referenced after line 366. The `.clone()` allocates a fresh String copy on every RRF search. A plain move works here — `sanitized` would be dropped on the `else` branch anyway since `expanded` is taken. Runs on every RRF-enabled search (the default path). A typical query string is ~30-100 bytes; over 1000 queries that's ~100KB of allocator churn, but more importantly it's a zero-cost fix.
- **Suggested fix:** `let fts_query = if expanded.is_empty() { sanitized } else { expanded };` Drop `.clone()`. The surrounding block owns `sanitized`; no borrow escapes.

---

## Resource Management

#### [RM-V1.29-1]: `load_references` rebuilds a 4-thread rayon pool + reloads every reference from disk on every `--include-refs` search
- **Difficulty:** medium
- **Location:** `src/cli/batch/handlers/search.rs:286`, `src/reference.rs:204-217`
- **Description:** The daemon search handler at `batch/handlers/search.rs:286` calls `cqs::reference::load_references(&config.references)` on every `--include-refs` search. That function (`reference.rs:204`) builds a fresh `rayon::ThreadPoolBuilder::new().num_threads(4).build()` **each call**, then reopens every reference Store (~16MB mmap) and HNSW index (~50-200MB) before the search — then drops everything at function scope exit. `BatchContext::refs` holds an LRU cache of `ReferenceIndex` specifically to avoid this, but `ctx.get_ref(...)` is only wired into explicit `--ref` queries, not the `--include-refs` multi-ref merge path. Impact: on a project with 3 references, every `cqs search --include-refs "q"` against the daemon pays 4 OS-thread spawn + teardown + 3 × (16MB mmap + 50-200MB HNSW load) ≈ 500ms-1s of pure I/O/TLB churn that the cache was designed to eliminate. The docstring above `refs: RefCell<LruCache<String, ReferenceIndex>>` in `batch/mod.rs:273` even says `Reduced from 4 to 2 — each ReferenceIndex holds Store + HNSW (50-200MB)` — but the `--include-refs` hot path never consults it.
- **Suggested fix:** Add `BatchContext::get_all_refs(&self) -> Result<Vec<Arc<ReferenceIndex>>>` that populates/reads from `self.refs` with one LRU miss per name (not a whole reload pass), then change `handlers/search.rs:286` to call it. Drop the sequential-fallback rayon pool construction — use the default global pool (which is what `par_iter()` at `:290` already does after `pool.install`, once the pool is unnecessary).

#### [RM-V1.29-2]: `evict_global_embedding_cache_with_runtime` opens `QueryCache` with a fresh single-thread tokio runtime every eviction tick
- **Difficulty:** easy
- **Location:** `src/cli/batch/mod.rs:1225`
- **Description:** Once per hour the daemon calls `evict_global_embedding_cache_with_runtime`. The EmbeddingCache half uses `open_with_runtime(&cache_path, runtime)` and reuses the shared daemon runtime — correct. But the QueryCache half at `:1225` calls `cqs::cache::QueryCache::open(&q_path)` (not `open_with_runtime`), which spawns a new `tokio::runtime::Builder::new_current_thread()` runtime (cache.rs:1015) just to run one `DELETE` SQL, then drops it. Every hour, on a daemon that already has `self.runtime` available right above, a fresh tokio runtime is spun up, used for ~10ms of SQL, and torn down. Not catastrophic but the asymmetry is against the `#968` runtime-sharing design documented two lines above in the same function. Small FD churn + wasted thread init on every tick.
- **Suggested fix:** Use `cqs::cache::QueryCache::open_with_runtime(&q_path, Some(runtime.clone()))` mirroring the EmbeddingCache path above. The `runtime` parameter is already threaded in.

#### [RM-V1.29-3]: `search_across_projects` builds a fresh rayon pool on every `cqs project search` invocation
- **Difficulty:** medium
- **Location:** `src/project.rs:217`
- **Description:** `search_across_projects` (called from `cli/commands/infra/project.rs:128`) rebuilds `rayon::ThreadPoolBuilder::new().num_threads(4).build()` every call. Unlike `load_references`, this is a per-CLI-invocation command so the per-build overhead is bounded to one build — but the fallback behavior is hidden: on pool creation failure it silently falls back to sequential, which the user can't distinguish from a slow-networked project store. The thread pool builder also doesn't set `thread_name`, so `ps` / `journalctl` show anonymous rayon worker threads. More importantly, for daemon mode (when someone wires this through batch handlers, eventually) this pattern will leak 4 threads per call with no LazyLock caching — the issue will recur.
- **Suggested fix:** Cache the pool via `OnceLock<Arc<rayon::ThreadPool>>`, or use the default global rayon pool with `par_iter()` directly. Either way, thread name each worker via `.thread_name(|i| format!("cqs-projects-{i}"))` so operators can spot it. Same applies to the sibling constructor in `reference.rs:204`.

#### [RM-V1.29-4]: `TelemetryAggregator::query_counts` grows unbounded per unique query — no cardinality cap
- **Difficulty:** medium
- **Location:** `src/cli/commands/infra/telemetry_cmd.rs:246`, `:282-290`
- **Description:** The aggregator starts with `query_counts: HashMap::with_capacity(64)` but inserts every distinct query string it encounters (`:287 insert(q.clone(), 1)`), with no cap. Telemetry archives can span months. If an agent-generated fuzzing loop, red-team session, or eval baseline run fires thousands of unique queries, the aggregator keeps one HashMap entry (average ~40 bytes key + 8 bytes count) for each. On a 365-day archive with one distinct query per minute (conservative for an AI agent driving cqs), that's ~525k entries ≈ 25MB just for `query_counts`. `finish()` sorts that by count and truncates at top-10, but the full accumulator is held in RAM during the pass. `cqs telemetry --all` then aggregates across ALL archived files serially.
- **Suggested fix:** Bound `query_counts` with a bounded-top-k heap (keep top-N queries by count, drop the rest), or cap the HashMap at e.g. 10_000 entries and evict the lowest-count entry on overflow. `top_queries` only uses top-10 anyway, so the full distribution is never surfaced. Even a coarse `query_counts.len() > 100_000` bail → tracing::warn would be better than unbounded growth.

#### [RM-V1.29-5]: CHM and WebHelp page readers use `std::fs::read` with no per-page byte cap
- **Difficulty:** easy
- **Location:** `src/convert/chm.rs:107`, `src/convert/webhelp.rs:120`
- **Description:** Both `CHM` (`chm.rs:107`) and `WebHelp` (`webhelp.rs:120`) page converters use `std::fs::read(entry.path())` to load a page's bytes, then `String::from_utf8_lossy`. Per-file cap is `MAX_FILE_SIZE = 100MB` on the outer archive (via `convert/mod.rs:363`), but individual extracted pages inside are read unconditionally. A malicious or pathological archive containing a single 500MB "page" (or one decompressed to large size) silently allocates the full file plus a UTF-8 transcoded copy. `convert/webhelp.rs:117` does have `MAX_WEBHELP_BYTES = 50 MB` on the outer webhelp bundle, but per-page it's still unbounded. On a 16-core WSL box with 32GB RAM this can still OOM for contrived cases — more importantly, no `tracing::warn` fires to attribute the OOM.
- **Suggested fix:** Use `std::fs::File::open(...).take(CAP).read_to_end(&mut buf)` per page. Cap at e.g. `MAX_CONVERT_PAGE_BYTES = 10 * 1024 * 1024` (sourced from a `limits.rs` helper, following the convention). Warn on truncation naming the file.

#### [RM-V1.29-6]: `cqs serve` multi-thread runtime has no worker_threads cap — uses `num_cpus` by default
- **Difficulty:** easy
- **Location:** `src/serve/mod.rs:63-66`
- **Description:** `run_server` builds `tokio::runtime::Builder::new_multi_thread().enable_all().build()` with no `.worker_threads(N)` call. Tokio default = num_cpus. On a 32-core A6000 workstation (user's setup) that's 32 worker threads for a read-only HTTP server that serves cached HNSW graph JSON. Compare to the daemon's `build_shared_runtime` at `watch.rs:114-123` which explicitly caps at 4 workers. The watch path has a comment explaining why (`one shared pool replaces three separate per-struct runtimes that previously idled ~6–12 OS threads`); `serve` runs without that discipline. Each idle tokio worker is ~2MB stack + scheduler state → ~64MB just to serve a likely ~1 req/sec endpoint.
- **Suggested fix:** `.worker_threads(std::thread::available_parallelism().map(|n| n.get().min(4)).unwrap_or(2))` mirroring `build_shared_runtime`. Also `.thread_name("cqs-serve-rt")` so the threads are identifiable in `top`/`htop`.

#### [RM-V1.29-7]: `EmbeddingCache` and `QueryCache` have no `Drop` impl → no WAL checkpoint on daemon shutdown
- **Difficulty:** medium
- **Location:** `src/cache.rs:42-65` (EmbeddingCache), `:968-977` (QueryCache)
- **Description:** Audit triage v1.29.0-pre P2 #70 was listed as `✅ PR #1045` fixed, but a fresh grep for `impl Drop for EmbeddingCache|impl Drop for QueryCache|pool.close()|wal_checkpoint.*cache` in `src/cache.rs` returns zero matches. `Store` has a proper `Drop` with `PRAGMA wal_checkpoint(TRUNCATE)` at `store/mod.rs:1303-1333`; the two caches do not. On `systemctl stop cqs-watch` (SIGTERM), the shared runtime + caches all drop; the SqlitePool's own drop closes connections cleanly but never issues a checkpoint. The WAL files at `~/.cache/cqs/embeddings.db-wal` and `~/.cache/cqs/query_cache.db-wal` survive across daemon restarts and accumulate until the next opportunistic checkpoint. On an agent workstation with many daemon restarts this grows to hundreds of MB over weeks.
- **Suggested fix:** Add `impl Drop for EmbeddingCache` / `impl Drop for QueryCache` mirroring `Store::drop` — `catch_unwind` around `rt.block_on(wal_checkpoint(TRUNCATE))`. Or call `self.rt.block_on(...)` inside a `close(self)` method and invoke it from the daemon's shutdown path in `watch.rs` right before the socket guard drops.

#### [RM-V1.29-8]: `Box::leak` pattern in watch.rs tests intentionally leaks parser/embedder/model on every test run
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:2660-2663` and `:2686-2689`
- **Description:** Two test helpers do `Box::leak(Box::new(CqParser::new().unwrap()))` plus three more `Box::leak` calls per test (embedder OnceLock, ModelConfig, RwLock gitignore). Under cargo nextest / parallel test runners, each test invocation leaks ~500KB (parser tables) + 16-32 bytes (OnceLock) + ModelConfig. Single run it's negligible; CI loops that run the test suite hundreds of times per hour accumulate. More importantly, these aren't `#[cfg(test)]`-gated at the module level — they're `fn test_cfg()` helpers that `cfg(test)` gates per-callsite, so they ship into the build artifact metadata. A careless `pub fn test_cfg` exposure (or a proc-macro shift) would leak in production. The leaks are also load-bearing for the `&'static` borrow pattern; the right idiom is a test-scoped `OnceLock<T>` or `std::sync::LazyLock`.
- **Suggested fix:** Replace `Box::leak` with `std::sync::LazyLock::new(|| ...)` at module scope (test-cfg'd). LazyLock holds the reference statically, no leak. Or use `&*Box::leak` inside a `#[test]`-scoped closure so the leak is per-test and the test runner can detect growing heap via valgrind.

#### [RM-V1.29-9]: Daemon socket thread spawn on every accept without pre-bounded stack size
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:1547-1552`
- **Description:** `std::thread::Builder::new().name("cqs-daemon-client".to_string()).spawn(...)` uses the default 2MB stack per thread (Linux glibc default). With `MAX_CONCURRENT_DAEMON_CLIENTS=16` default, worst-case is 32MB of stack just for accept handlers — but the cap can be overridden via `CQS_MAX_DAEMON_CLIENTS` to any value. At `CQS_MAX_DAEMON_CLIENTS=128` (e.g. a heavy agent fanout) we allocate 256MB of stacks for connection handlers that do ~1KB of actual work each. The handler itself is shallow (BufReader + dispatch + write), so 128KB stack would suffice.
- **Suggested fix:** `std::thread::Builder::new().name(...).stack_size(256 * 1024).spawn(...)`. Caps worst-case stack at 32MB even with a 128-client cap. Test under miri or valgrind to confirm 256KB is enough for the `handle_socket_client` deepest call stack (should be trivially so given the non-recursive design).

#### [RM-V1.29-10]: `handle_socket_client` BufReader allocates capacity sized to the 1MB `.take()` cap on every connection
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:194`
- **Description:** `let mut reader = std::io::BufReader::new(&stream).take(1_048_577);` creates a new BufReader per accepted connection, with the default 8KB internal buffer. That's fine — but the unused 1MB cap is consumed whenever someone sends a large (but valid) request, and the BufReader itself is allocated + dropped per connection with no reuse. On burst agent traffic (100s of queries/sec on a dispatch-heavy batch loop) this is ~10MB/sec of allocator churn from per-connection buffer alloc + grow. Separately, `line` is `String::new()` at `:195` — it grows as-needed up to 1MB, then gets dropped. No per-thread recycle.
- **Suggested fix:** Thread-local `Cell<String>` as a scratch buffer (like the `parallel_read_buf` pattern), clear + reuse per connection. Low priority — only matters at high request rates; cqs is currently bottlenecked elsewhere. Call it a scaling-headroom fix.

## Summary
Found 10 resource-management issues in v1.29.0: (1) reference cache bypass in `--include-refs` path — worst offender, reloads ~500MB of Store+HNSW per daemon query; (2) QueryCache eviction spins a fresh tokio runtime hourly instead of reusing the shared daemon runtime; (3-4) rayon thread-pool rebuilds per call in `search_across_projects` / `load_references`; (5) unbounded `query_counts` HashMap in telemetry aggregation; (6) missing per-page byte caps in CHM/WebHelp converters; (7) `cqs serve` runtime uncapped at num_cpus workers; (8) missing WAL checkpoint on EmbeddingCache/QueryCache Drop despite P2 #70 claim; (9) `Box::leak` in test helpers; (10) default 2MB stack for up-to-128 daemon client threads; (11) per-connection BufReader allocator churn in watch daemon.

---

## Test Coverage (happy path)

#### TC-HAP-1.29-1: `cqs serve` data endpoints (`build_graph`, `build_chunk_detail`, `build_hierarchy`, `build_cluster`) never tested with populated data
- **Difficulty:** medium
- **Location:** `src/serve/data.rs:192` (`build_graph`), `:452` (`build_chunk_detail`), `:586` (`build_hierarchy`), `:825` (`build_cluster`), `:933` (`build_stats`). All tests are in `src/serve/tests.rs` and use `fixture_state()` at line 25 which creates an empty init-only store.
- **Description:** The entire `cqs serve` subsystem (new in v1.29.0) has 14 endpoint tests, but every test runs against an empty store. Result: `graph_returns_empty_for_fresh_store` asserts `nodes.len() == 0`, `chunk_detail_unknown_id_returns_404` asserts 404, `hierarchy_unknown_root_returns_404` asserts 404, `cluster_returns_empty_for_fresh_store` asserts `nodes.len() == 0`. The test file comment at line 417-419 (`graph_returns_empty_for_fresh_store`) even admits this: *"Real graph rendering is exercised by manual smoke against the cqs corpus; an in-process test would need a populated fixture (~few hundred LOC of chunk inserts) which is more setup than the shape-check is worth at this stage."* The SQL queries in `build_graph` (cf. `src/serve/data.rs:219-260` correlated subquery for n_callers, edge resolution at `:300-440`), the BFS in `build_hierarchy` (`:620-655`), UMAP-coord lookup in `build_cluster` (`:829-924`), and the caller/callee enrichment in `build_chunk_detail` (`:452-577`) are all untested with actual chunks. A mistake in any of these (e.g., filter-by-file bug, max_nodes clamp off-by-one, direction=callers/callees confusion) ships silently — there is no positive test that asserts "if you index 3 chunks with 2 call edges, `/api/graph` returns 3 nodes and 2 edges".
- **Suggested fix:** Add a module in `src/serve/tests.rs` (or new file) that uses `common::InProcessFixture`-style chunk seeding to populate the store with a small call graph (e.g. `process_data` → `validate` → `format_output`, plus a test chunk). Then assert:
  - `build_graph` returns 3 data nodes + 2 call edges; `max_nodes=1` truncates; `file_filter` narrows; `kind_filter="function"` excludes tests.
  - `build_chunk_detail(store, "process_data_chunk_id")` returns `callers.len()==0, callees.len()==2, tests.len()==1`.
  - `build_hierarchy(store, root, Direction::Callees, depth=5)` returns the 3-node subtree; direction=Callers returns just the root; depth=1 truncates.
  - `build_cluster` returns N nodes with coords when UMAP coords are populated; returns `skipped=N` when they are not.

#### TC-HAP-1.29-2: Batch dispatch handlers (`dispatch_gather`, `dispatch_scout`, `dispatch_task`, `dispatch_where`, `dispatch_onboard`, `dispatch_callers`, `dispatch_callees`, `dispatch_impact`, `dispatch_test_map`, `dispatch_trace`, `dispatch_similar`, `dispatch_explain`, `dispatch_context`, `dispatch_deps`, `dispatch_related`, `dispatch_impact_diff`) have zero tests
- **Difficulty:** medium
- **Location:** `src/cli/batch/handlers/misc.rs:15, 131, 173, 209` (gather/task/scout/where); `src/cli/batch/handlers/graph.rs:24, 63, 103, 143, 233, 292, 375, 392` (deps/callers/callees/impact/test_map/trace/related/impact_diff); `src/cli/batch/handlers/info.rs:46, 100, 168, 302` (explain/similar/context/onboard).
- **Description:** Only `dispatch_search` has inline tests (5 of them in `src/cli/batch/handlers/search.rs:528-742`). The other **16** batch dispatch functions have zero tests. These are the daemon-hot-path handlers every agent hits via `cqs batch` and the socket path — a shape change to the JSON output or a regression in a chunk resolver bubbles silently until an agent notices. Grep confirms: no test file in `tests/` references any of these dispatch fns (only `real_eval_callgraph.json` mentions `dispatch_trace` as a pattern label). Batch 1 triage item P2 #48 addressed `dispatch_review/dispatch_diff/dispatch_drift/dispatch_blame/dispatch_plan`, but the read-only graph/info/search surface was not included.
- **Suggested fix:** Add a single integration file `tests/batch_handlers_test.rs` that uses `common::InProcessFixture` to seed a small corpus + call graph, then calls each handler through `BatchContext::dispatch_line("<cmd> <args>", &mut sink)` and asserts the JSON envelope's `data` field has the expected keys + a non-empty results array. One test per handler is enough. Example: `ctx.dispatch_line("callers process_data", &mut sink)` → JSON has `callers: [...]` with the seeded caller. Follows the exact same pattern `dispatch_search` tests already use (`create_test_context`, seeded chunks).

#### TC-HAP-1.29-3: `Reranker::rerank` and `Reranker::rerank_with_passages` have no tests
- **Difficulty:** medium
- **Location:** `src/reranker.rs:160` (`rerank`), `:190` (`rerank_with_passages`). Only `sigmoid` (a scalar helper) has tests (lines 450+).
- **Description:** `rerank`/`rerank_with_passages` are the two public entry points of the cross-encoder re-ranking subsystem — they take a `Vec<SearchResult>` + query and rescore via ORT. Zero tests pin their contract. The only callers in `tests/` are `eval_harness.rs:527` and `model_eval.rs:1417`, both of which use the reranker as a black box inside an evaluation loop and neither asserts behaviour of a specific (query, passages) pair. A regression in the pair-encoding (batch concat, attention mask), the sigmoid mapping to scores, or the ORT session call would surface as "eval scores moved slightly" rather than a unit-test failure. Given the reranker was flagged in recent audits as a correctness-critical scoring component (see P3 #100 "reranker over-retrieval pool ... duplicated 4x"), its own surface needs at least one happy-path pin.
- **Suggested fix:** Add two tests (likely `#[ignore]`-gated because they need the reranker model on disk — same shape as the existing ignored model tests):
  - `test_rerank_preserves_input_set_reorders_by_score`: seed 3 passages with obviously different relevance to a query ("rust async await" → ["tokio runtime docs", "how to bake sourdough", "rust futures trait"]); assert all 3 are returned and the baking passage ranks last.
  - `test_rerank_with_passages_empty_input_returns_empty_output`: pin the no-op shortcut. (This is the one test a non-model-loading run could cover.)

#### TC-HAP-1.29-4: `cmd_project { Register, Remove, Search }` — only `Register/List/Remove` has a CLI test; `Search` has no CLI integration test
- **Difficulty:** medium
- **Location:** `src/cli/commands/infra/project.rs:70` (`cmd_project`). Existing CLI tests: `tests/cli_surface_test.rs:170` `project_register_list_remove_round_trips`, `:252` `project_remove_nonexistent_succeeds_quietly`, `tests/cli_envelope_test.rs:133` `cqs --json project search anything`.
- **Description:** The `Search` subcommand (the one that does actual cross-project semantic search via `search_across_projects`, lines 119-164) runs `Embedder::new` → `embed_query` → `search_across_projects` → emit. The only existing test `tests/cli_envelope_test.rs:133` invokes `cqs --json project search anything` but asserts **only** the envelope shape, not that results were returned. There is no test that registers two projects, indexes both, runs `project search <query>`, and asserts results from both projects are interleaved and tagged with their project name. Inline tests in `src/cli/commands/infra/project.rs:178-207` only exercise `ProjectSearchResult` JSON serialization, not the end-to-end path. This is the ONLY cross-project-search surface — a regression (wrong project_name tag, dedup across projects collapsing results, weight application across indexes) ships silently.
- **Suggested fix:** Add `tests/cli_project_search_test.rs` with one test that uses `InProcessFixture` to create two temp stores (projects), writes distinct content in each, invokes `cqs project register` twice, then `cqs project search "<term>"`, and asserts: (a) at least one result comes from each project, (b) the `project` field on each result matches the registered name, (c) exit code 0. Can share the cross-project fixture from `tests/cross_project_test.rs`.

#### TC-HAP-1.29-5: `cmd_ref_add`, `cmd_ref_list`, `cmd_ref_remove`, `cmd_ref_update` (CLI) have no end-to-end test; only library-level helpers tested
- **Difficulty:** medium
- **Location:** `src/cli/commands/infra/reference.rs:88, 187, 320, 350` (add/list/remove/update). Tests in `tests/reference_test.rs` (397 lines, 11 tests) exercise `merge_results`, `search_reference`, `validate_ref_name` — all library functions. `tests/cli_drift_diff_test.rs:114` calls `cqs ref add` as setup for drift/diff tests but asserts only the drift output, not the ref add shape.
- **Description:** The 4 CLI subcommands under `cqs ref` do real work: `add` runs `enumerate_files` + `run_index_pipeline` + `build_hnsw_index` + `add_reference_to_config` (see lines 128-179); `list` reads every reference's DB for chunk counts (lines 187-280); `remove` validates existence + rewrites config (lines 320-340); `update` re-indexes from source. None have a happy-path CLI test. Impact: a regression in the TOML config round-trip (`add_reference_to_config` at `src/config.rs`), or in HNSW-per-reference path, or in the chunk-count aggregation shown by `list`, would ship unnoticed. The drift/diff tests use `ref add` but only as setup — they don't validate its output shape.
- **Suggested fix:** Add `tests/cli_ref_test.rs` with 4 tests:
  - `ref_add_then_list_shows_reference_with_chunk_count` — add a tiny reference (2 files), then `ref list --json` and assert `[{name, path, chunks: >=2}]`.
  - `ref_remove_deletes_from_config_and_disk` — add, then remove, then `ref list` returns empty.
  - `ref_update_reindexes_source_content` — add, modify a source file, `ref update`, assert chunk count changed.
  - `ref_add_weight_rejects_out_of_range` — pin `0.0..=1.0` contract (already tested at library level via `validate_ref_name` but not CLI-level).

#### TC-HAP-1.29-6: `cqs batch` daemon socket (`handle_socket_client`) — no happy-path test that a valid command round-trips through the socket
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:160` (`handle_socket_client`). Referenced by Batch 1 TC-ADV-1.29-3 for adversarial cases; this finding is the happy-path complement. `tests/daemon_forward_test.rs` has 9 tests but only covers the CLI-forwarding translator (`translate_cli_args_to_batch`) and `ping` — not a real dispatch-line round trip.
- **Description:** `handle_socket_client` is the daemon hot-path — every systemd-deployed `cqs watch --serve` instance serves ALL agent queries through it. The 9 tests in `daemon_forward_test.rs` cover only the pre-socket translation layer + `ping`. There is no test that sends `{"command":"search","args":["foo"]}` down a `UnixStream` pair, reads the response, and asserts `{data: {results: [...], total: N}}`. A regression in the framing layer (newline vs length-prefix, NUL-termination, gzip wrap), the JSON envelope wrap (P2 #28 already overlapped this for CLI; socket path is distinct), or the per-command dispatch registration in `dispatch_socket_command` would not surface until an agent noticed a broken batch response. The existing `tests/daemon_forward_test.rs:321` `test_mock_socket_round_trip_for_daemon_command` is labelled "mock socket" — it's actually a one-sided harness; see the file for details.
- **Suggested fix:** Add `tests/daemon_socket_roundtrip_test.rs` that creates a `tokio::net::UnixStream::pair()`, spawns `handle_socket_client` against the server half with a minimal `BatchContext` + seeded store, writes a newline-terminated JSON request, reads the response, and asserts envelope shape + payload. Cover one read-only command (e.g. `stats` — no embedder needed) to avoid model load cost.

#### TC-HAP-1.29-7: `cmd_similar` (CLI) has no integration test; only inline serialization tests and module-level `find_similar` tests
- **Difficulty:** easy
- **Location:** `src/cli/commands/search/similar.rs:41` (`cmd_similar`). Inline tests at line 280+ test output struct serialization only (5 tests, none exercise `cmd_similar`). Library `find_similar`/`find_related` is tested in `tests/related_impact_test.rs`.
- **Description:** `cmd_similar` is the CLI entry point for "find functions similar to X by embedding distance". No test invokes it with real input. The library `find_similar` is tested but `cmd_similar` adds: target-function lookup (`store.search_by_name`), embedding fetch, pattern filter application (line 72+), and typed-output JSON build. Each is a regression surface. Very cheap to add because `InProcessFixture` already has the scaffolding and `find_similar` can use `MockEmbedder`.
- **Suggested fix:** Add one test in `src/cli/commands/search/similar.rs::tests` using in-process fixture: seed 3 functions, call `cmd_similar(&ctx, "foo")`, capture stdout via a `Write` sink (the fn currently uses `println!`; if that makes it hard, add to the JSON path only — `--json` sinks via `emit_json`). Assert output contains the other 2 as similar results.

#### TC-HAP-1.29-8: `cmd_ci` happy path — library `ci::analyze_diff` tested, CLI function only tested in error paths
- **Difficulty:** easy
- **Location:** `src/cli/commands/review/ci.rs:9` (`cmd_ci`). Tests in `tests/ci_test.rs` cover library `ci::analyze_diff` (happy path: yes); `tests/cli_train_review_test.rs` has CLI tests for `cmd_ci` but they are error-path only (P2 #46 deliverable — see the batch 1 audit summary).
- **Description:** The CLI surface for `cqs ci` (which produces the GitHub-Actions-style review comment) runs: diff parsing → `analyze_diff` → `cqs_format` / `markdown_format` output → exit code assignment. `cli_train_review_test.rs` (line 154+) has a comment "P2 #46 (b) — `cmd_task`" and covers only failure modes. There is no test that feeds a real diff into `cmd_ci` and asserts the output markdown contains the expected sections ("High-risk changes", "Tests"). A regression in markdown formatting or exit-code mapping (lines 47-95 in ci.rs) ships silently.
- **Suggested fix:** Add one happy-path test in `tests/cli_train_review_test.rs` or a new `cli_ci_test.rs`: feed a real unified-diff string (modify a known indexed function) to `cmd_ci`, assert stdout contains "High-risk" when the diff touches a hotspot and the exit code matches the severity level set at `cli/commands/review/ci.rs:62-87`.

#### TC-HAP-1.29-9: `cmd_gather` (CLI) untested; only library `gather()` tested
- **Difficulty:** easy
- **Location:** `src/cli/commands/search/gather.rs:77` (`cmd_gather`). Inline tests at bottom cover output shape; `tests/gather_test.rs` tests library `gather()`. CLI-level `cmd_gather` has zero integration tests.
- **Description:** `cmd_gather` adds BFS depth / max_files clamping, content injection via `inject_content_into_gather_json` (unique to CLI path, not library), token budget trimming via `apply_token_budget`, and the JSON envelope wrap. A regression in any of these silently alters the output agents rely on for `/investigate` and `cqs task`. Library tests don't exercise these CLI-only steps.
- **Suggested fix:** Add one test in `tests/cli_gather_test.rs` (or reuse `tests/gather_test.rs` harness with `cqs()` spawning): seed a 3-function corpus, run `cqs gather "foo" --json --max-files 2`, assert the response envelope has `results.len() == 2` (clamp worked), `tokens_in/tokens_out` fields present when `--tokens N` passed, content injected into each file_group.

#### TC-HAP-1.29-10: `dispatch_line` has no happy-path test that verifies a valid command produces a correct JSON response
- **Difficulty:** easy
- **Location:** `src/cli/batch/mod.rs:557` (`dispatch_line`). Tests at lines 1908 (`bumps_query_counter` with a `bogus` input — error), 2124 / 2161 / 2198 (all adversarial — NUL bytes, unbalanced quotes).
- **Description:** Every existing `dispatch_line` test uses either a parse-failure input (`bogus`) or an adversarial malformed input. There is no test that sends a valid `search foo` / `stats` / `dead` down `dispatch_line`, reads the `Vec<u8>` sink, parses the JSON, and asserts envelope shape. The tests pin error_count/query_count invariants but not the actual output — a regression that produced the error envelope where it should have produced a success envelope would still pass the counter checks. Relatedly, `dispatch_search` is well-covered at the **handler** level but the **dispatch-line** wrap (clap parsing → handler routing → envelope serialization → newline emission) is not.
- **Suggested fix:** Add one test in `src/cli/batch/mod.rs::tests`: `test_dispatch_line_stats_emits_success_envelope` — run `ctx.dispatch_line("stats", &mut sink)` against an init-only store, parse the `sink` bytes as JSON, assert `json["data"]["total_chunks"].is_number()` and `json["error"].is_null()`. Small. Would catch P2-class regressions (envelope-shape changes, stats serialization drift).

## Summary

10 findings filed. The most impactful gap is #1 — the entire `cqs serve` subsystem (v1.29.0 flagship feature) has 14 tests but all run against an empty store, so none of `build_graph` / `build_chunk_detail` / `build_hierarchy` / `build_cluster` are actually validated for correctness. Runners-up: 16 untested batch dispatch handlers (#2), no positive round-trip test for the daemon socket (#6), and the `Reranker`/`cmd_project search`/`cmd_ref_*` core functionality all lack happy-path CLI or integration coverage. Several of these are cheap adds (~1 test each) because the in-process fixture already exists.

---
