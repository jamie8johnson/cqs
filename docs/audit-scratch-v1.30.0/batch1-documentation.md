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
