# Audit Findings (post-v1.38.0, pre-#1513 archive)

Fresh audit of cqs at main, ~PR #1511. Based on 16 parallel auditor agents (8 per batch).

**Total findings: 154** — 76 in batch 1, 78 in batch 2.

| Category | Count | Marker |
|---|---|---|
| Code Quality | 6 | CQ |
| Documentation | 10 | DOC |
| API Design | 10 | API |
| Error Handling | 10 | EH |
| Observability | 10 | OB |
| Test Coverage (adversarial) | 10 | TC-ADV |
| Robustness | 10 | RB |
| Scaling & Hardcoded Limits | 10 | SHL |
| Algorithm Correctness | 10 | AC |
| Extensibility | 10 | EX |
| Platform Behavior | 10 | PL |
| Security | 10 | SEC |
| Data Safety | 8 | DS |
| Performance | 10 | PF |
| Resource Management | 10 | RM |
| Test Coverage (happy path) | 10 | TC-HAP |

All findings labelled with `V1.38-N` to anchor to this audit cycle.

---


## Code Quality

## Code Quality

Audit base: origin/main @ `4cfe82ae` (post-v1.38.0 + #1507/#1508/#1509/#1510). Local checkout was 4 commits behind during audit; findings verified against `git show origin/main` to avoid stale-tree noise.

Prior audit `audit-findings.md` (v1.36.2) findings still applicable / unfixed at audit time: CQ-V1.36-1 (enrichment hot-path drops `model_max_seq_len`), CQ-V1.36-2 (FIXED in #1473), CQ-V1.36-3/5/6 (FIXED in #1473), CQ-V1.36-4 (stale dead-code index — STILL APPLICABLE; see findings below), CQ-V1.36-7 (FIXED — `saturating_mul(3)` is in `src/index.rs:70` on origin/main).

The 96 src/-only entries in `cqs dead --json` are mostly stale-index ghosts (`languages.rs` macro expansions, test helpers under `#[cfg(test)]`, `serde(default = "fn_name")` attribute references, signal-handler `extern "C" fn`s, derive-macro-referenced helpers). The findings below are the genuine residue after that filtering.

#### CQ-V1.38-1: `parse_env_duration_secs` parked as `#[allow(dead_code)]` for "follow-up" callers that never landed
- **Difficulty:** easy
- **Location:** `src/limits.rs:417-429`
- **Description:** `pub fn parse_env_duration_secs(key, default_secs) -> Duration` is `#[allow(dead_code)]` with a comment saying call sites "live in agent-D-owned modules and will land in a follow-up". `git grep parse_env_duration_secs` returns one hit (the definition); zero callers across `src/` and `tests/`. The "follow-up" never happened. Centralizing the env contract is the stated motive, but with no consumer the helper is just an unindexed tombstone — and worse, the next person who needs `parse_env_u64(...) -> Duration::from_secs` will likely reinvent the helper rather than discover it via search-by-name.
- **Suggested fix:** Either delete the function (project policy: no external users, no deprecation cycle) or wire it into `daemon_periodic_gc_*_secs()` / `serve_idle_minutes()` / `daemon_reconcile_interval_secs()`, all of which currently do `Duration::from_secs(parse_env_u64(...))` inline at their call sites and would benefit from the centralized helper.

#### CQ-V1.38-2: `open_project_store()` (zero-arg, no slot) is `#[allow(dead_code)]` with no callers — superseded by slot-aware variant
- **Difficulty:** easy
- **Location:** `src/cli/store.rs:102-112`
- **Description:** `pub(crate) fn open_project_store() -> Result<(Store, PathBuf, PathBuf)>` is `#[allow(dead_code)]` and the doc comment admits "Kept for legacy in-tree callers that don't (yet) flow through `CommandContext`". `git grep '\bopen_project_store\b'` against origin/main shows zero non-doc, non-self-reference call sites. The counterpart `open_project_store_for_slot(slot_flag: Option<&str>)` (line 116) and `open_project_store_readonly` (line 126) are the ones actually used. Keeping the dead variant `pub(crate)` invites a new caller that would silently bypass `--slot` resolution — the same shape as the configurable-models / SPLADE-config drops the v1.36.2 audit found.
- **Suggested fix:** Delete `open_project_store()`. Any future caller needs the slot-aware variant. Update line 123 doc reference (`Same as [open_project_store] but uses ...`) on `open_project_store_readonly` to point at `open_project_store_for_slot` since the deleted function will no longer exist.

#### CQ-V1.38-3: `RISK_THRESHOLD_HIGH` / `RISK_THRESHOLD_MEDIUM` re-exports are `#[allow(dead_code)]` with zero crate-wide callers
- **Difficulty:** easy
- **Location:** `src/impact/hints.rs:21-32`
- **Description:** `pub const RISK_THRESHOLD_HIGH: f32 = RISK_THRESHOLD_HIGH_DEFAULT` (and the MEDIUM sibling) are `#[allow(dead_code)]` with the doc comment "Retained as a public constant for callers that need the compile-time default (docs, tests, telemetry)". `grep -rn 'RISK_THRESHOLD_HIGH\b' src/ tests/` (excluding the `_DEFAULT` versions in `limits.rs` which production code uses directly) returns zero callers. The `_DEFAULT` constants in `limits.rs` are already `pub(crate)` and accessible everywhere they need to be; the `_DEFAULT`-less aliases in `impact/hints.rs` add a third name for the same value with no consumer.
- **Suggested fix:** Delete the two `pub const RISK_THRESHOLD_*` aliases on `src/impact/hints.rs:28,32`. Production paths use `risk_threshold_high()` / `risk_threshold_medium()` (the env-aware accessors) and the `_DEFAULT` constants are already exposed at `pub(crate)` for any future test pin.

#### CQ-V1.38-4: `parser::MAX_FILE_SIZE` / `MAX_CHUNK_BYTES` re-exports are `pub(crate)` `#[allow(dead_code)]` with no callers and a doc claim of "downstream crates"
- **Difficulty:** easy
- **Location:** `src/parser/mod.rs:35-39, 65-69`
- **Description:** Both constants alias `crate::limits::PARSER_MAX_*` with `#[allow(dead_code)]` and a doc comment "for the legacy `tc35_max_file_size_is_50mb` test pin and for downstream crates that still re-export it." But the constants are `pub(crate)`, so no downstream crate can see them, and the test in question (`tc35_max_file_size_is_50mb`) doesn't exist anywhere in the tree (`grep -rn tc35_max_file_size_is_50mb` returns zero hits). Net: dead aliases whose justification doesn't match their visibility or any active test.
- **Suggested fix:** Delete both constants. Production reads `crate::limits::parser_max_file_size()` / `parser_max_chunk_bytes()` (env-aware) and tests can pin `crate::limits::PARSER_MAX_FILE_SIZE` directly if a future regression demands it.

#### CQ-V1.38-5: `ScoringConfig::DEFAULT` and `SCORING_KNOBS` carry duplicate hardcoded defaults with no compile-time or runtime sync check
- **Difficulty:** medium
- **Location:** `src/search/scoring/config.rs:30-47`, `src/search/scoring/knob.rs:55-152`
- **Description:** Score-tier defaults are encoded twice: as fields on `pub const ScoringConfig::DEFAULT: Self` and as `default:` values on the corresponding rows of the `SCORING_KNOBS` slice. The doc comment on each is explicit ("mirror the `ScoringConfig::DEFAULT` consts" / "Mirrors the `default` column on each score-tier row"), but nothing enforces the mirror — no `const_assert!`, no `#[test]` that iterates `SCORING_KNOBS` and compares against `ScoringConfig::DEFAULT`. A drive-by edit changing `name_exact: 1.0` in one place but not the other compiles and tests-pass; production scoring would silently use one default while the resolver / `cqs doctor` reported the other. There are 9 tiers × 2 sources = 18 numbers that must hand-stay-in-lockstep.
- **Suggested fix:** Add a `#[test] fn scoring_knob_defaults_match_scoring_config_default()` in `src/search/scoring/config.rs` that asserts each knob's `default` equals the corresponding `ScoringConfig::DEFAULT` field (use a small lookup table). Or (preferred) generate one from the other — e.g. derive `ScoringConfig::DEFAULT` from `SCORING_KNOBS` via a `const fn`-shaped lookup so the slice is the single source of truth.

#### CQ-V1.38-6: `cqs dead` / HNSW chunk index serves stale chunk_id → file:line mappings, costing every audit ~30+ ghost-investigations
- **Difficulty:** medium (tooling)
- **Location:** Tooling (HNSW + dead-code analyzer), affects audit/triage workflow.
- **Description:** This is the same workflow problem CQ-V1.36-4 flagged. Per-file dead-code inspection on origin/main: `submit_doc_batch` / `submit_hyde_batch` (still flagged at `src/llm/batch.rs:387,395`, `src/llm/local.rs:681,689`, `src/llm/provider.rs:153/161/207/215`) — all gone post-#1347 (only one doc-comment reference survives). Macros `define_languages` / `define_aux_presets` flagged at lines that don't match the current file. `cmd_completions`, `markdown_passthrough`, `default_output_name`, `default_model_name`, `default_ref_weight`, `is_zero_u32`, `on_sigterm`, `human_bytes`, `format_thousands`, `format_uptime`, `pattern_def_for`, `make_chunk_at` — all flagged dead but each has live callers (test helpers, `serde(default = ...)` references, signal handlers, derive-macro emit sites, `.unwrap_or_else(default_*)` chains). This isn't a code defect but it's a load-bearing tooling gap: the auditor must re-verify every dead-code hit by reading the current source, defeating the speed-up the command exists for.
- **Suggested fix:** Two layers. (1) Hot fix: re-run `cqs index` (or restart the watch service) so chunk_id mappings refresh — most of the false positives are just stale IDs from pre-#1473/#1347 trees. (2) Permanent fix: dead-code analyzer should attach `content_hash` to each `dead` entry and skip when the on-disk hash at `(file, line_start, line_end)` differs from the indexed hash. Equivalently, callers found via `serde(default = "fn_name")` string literals, `extern "C" fn` registered to `libc::signal`, and `#[derive(...)]` macro inputs should be treated as live.


---

## Documentation

# Documentation Audit — post-v1.38.0

Audit window: post-v1.36.2 triage (`docs/audit-triage-v1.33.0.md` was the most recent — no `audit-triage-v1.36.2-final.md` exists, so prior fixes were verified by reading current source). Recent merges examined: #1495 (proc-macro registry), #1497 + #1499 (skip-first-pass embed → schema v27), #1505 (`cqs index --model`), #1506 (`cqs ref reindex`), #1486 release notes, plus 8 audit close-out PRs that landed post-v1.38.0.

#### DOC-V1.38-1: README + lib.rs + Cargo.toml claim `--improve-docs` writes to source; default has been patch-mode since v1.30.1
- **Difficulty:** easy
- **Location:** `README.md:659` ("`cqs index --llm-summaries --improve-docs  # Generate + write doc comments to source files`"), `README.md:673` ("`--improve-docs` generates and writes doc comments back to source files"), `src/lib.rs:19` (Features bullet: "`--improve-docs` generates and writes doc comments to source files via LLM"), `SECURITY.md:48` (Surfaces table: "LLM output is **written back to source files in place**. A poisoned chunk can produce a doc comment that lands in the user's repo on commit"). Verified by `src/doc_writer/rewriter.rs:224, 607, 1204` — default writes patches under `.cqs/proposed-docs/<rel>.patch` and requires `--apply` to actually mutate source. SECURITY.md *itself* (line 55) correctly documents the v1.30.1 review-gate; lines 48 and the README/lib.rs claims contradict it.
- **Description:** This is a "lying doc" cluster — three top-level surfaces (README, crate-level rustdoc, SECURITY.md surfaces table) tell users / agents that a flag *will* mutate source files, when in fact the flag writes a unified-diff patch by default. SECURITY.md is the worst offender because the threat-model row claims a write that the mitigation row (#7 below it) explicitly disclaims. An agent reading SECURITY.md as authoritative would either over-fear the flag (and avoid using it) or under-fear `--apply`.
- **Suggested fix:** README.md:659 → `# Write proposed doc comments as patches under .cqs/proposed-docs/ (review with git apply); pass --apply to write directly`. README.md:673 → "`--improve-docs` writes proposed doc comments as patches under `.cqs/proposed-docs/` for review (pass `--apply` to write back to source files in place). Both cached by content_hash." SECURITY.md:48 row → change "Yes — commits the LLM's output into git" to "Patch-only by default (review-gate); `--apply` writes in place — see Mitigation #3 below". src/lib.rs:19 → "`--improve-docs` writes proposed doc comments as `.cqs/proposed-docs/*.patch` for review; `--apply` writes them in place".

#### DOC-V1.38-2: CONTRIBUTING.md still describes `src/cli/registry.rs` and `for_each_command!` — both deleted in #1495
- **Difficulty:** easy
- **Location:** `CONTRIBUTING.md:156-157` (architecture overview lists `registry.rs - for_each_command! table; single source of truth for dispatch...` and `dispatch.rs - ...per-command arms generated from registry.rs`), `CONTRIBUTING.md:367` (Adding a New CLI Command checklist step 4: "**Registry row** — add a `(bind, wild, name, batch_support, body)` row to `group_a` or `group_b` in `src/cli/registry.rs`. The `for_each_command!` macro generates dispatch + variant_name + batch_support from this single row; a missing row is a compile error."). Verified `ls src/cli/registry.rs` — file does not exist. `src/cli/definitions.rs:341` now reads `#[derive(Subcommand, cqs_macros::CqsCommands)]`; `src/cli/dispatch.rs:13-16` and `:209-213` describe the proc-macro replacement.
- **Description:** Lying doc — guides a contributor (human or agent) through a checklist that points to a file that doesn't exist. Anyone following the doc to add a new command will fail step 4 immediately, and the architecture overview misrepresents the dispatch mechanism. Triaged in v1.33.0 as "single-registration command registry"; the implementation got replaced by `#[derive(CqsCommands)]` proc-macro (`crates/cqs-macros/`) but the documentation never followed.
- **Suggested fix:** Replace the two arch-overview lines with `definitions.rs - Clap argument definitions, Commands enum, and #[derive(cqs_macros::CqsCommands)] which generates dispatch + variant_name + batch_support` and drop the `registry.rs` row entirely. Replace step 4 with: "**Derive surfaces** — `Commands` enum variants get dispatch + `variant_name()` + `batch_support()` from `#[derive(cqs_macros::CqsCommands)]` automatically. Per-variant attribute knobs: `#[cqs(handler = ...)]`, `#[cqs(batch_support = "yes")]`, etc. See existing variants in `src/cli/definitions.rs` and the macro in `crates/cqs-macros/`. A missing handler is a compile error from the derive expansion."

#### DOC-V1.38-3: CONTRIBUTING.md says "Schema v26" twice; current schema is v27 (#1497)
- **Difficulty:** easy
- **Location:** `CONTRIBUTING.md:212` ("`store/        - SQLite storage layer (Schema v26, WAL mode)`") and `CONTRIBUTING.md:226` ("`migrations.rs - Schema migration framework (v10-v26, including ... v26 composite (source_type, origin) index on chunks)`"). Verified: `src/store/helpers/mod.rs:151` reads `pub const CURRENT_SCHEMA_VERSION: i32 = 27;`. `src/schema.sql:1` reads `cq index schema v27`. `src/store/migrations.rs:398` registers `(26, 27, |c| Box::pin(migrate_v26_to_v27(c)))`. The v26→v27 migration (#1452 / #1497) added `chunks.needs_embedding INTEGER NOT NULL DEFAULT 0` plus a partial index, to support skip-first-pass embedding under `--llm-summaries`.
- **Description:** Stale schema citation — same flavour of the line-citation drift fixed in v1.37.0 (P1-12, where SECURITY.md cited v25 after v26 landed). New audit cycle, same drift. Anyone reading CONTRIBUTING.md to plan a migration will undercount versions and miss v27's needs_embedding column.
- **Suggested fix:** Line 212 → `store/        - SQLite storage layer (Schema v27, WAL mode)`. Line 226 → `migrations.rs - Schema migration framework (v10-v27, including v19 FK cascade, v20 trigger, v21 splade tokens, v22 chunks.umap_x/y, v23 reconcile fingerprint, v24 vendored-code trust, v25 notes.kind, v26 composite (source_type, origin) index on chunks, v27 chunks.needs_embedding for skip-first-pass embed under --llm-summaries (#1452))`.

#### DOC-V1.38-4: SECURITY.md cites `src/lib.rs:813` for `enumerate_files`; actual line is 771
- **Difficulty:** easy
- **Location:** `SECURITY.md:223` — "Symlinks are **skipped** entirely — `enumerate_files` / `enumerate_files_iter` (`src/lib.rs:813`, `WalkBuilder::follow_links(false)`)...". Verified by `grep -n "pub fn enumerate_files" src/lib.rs` → line 771. (v1.37.0 P1-12 already fixed this drift once — moving from `:601` to `:813`. The line has drifted again since.)
- **Description:** Stale line-number citation — third drift cycle on the same anchor (601 → 813 in v1.37.0; now 813 → 771 in v1.38.0+). This is exactly the kind of staleness the team rule on "lying docs as P1" cares about: an auditor cross-referencing SECURITY.md to verify the symlink-skip claim will read line 813, find an unrelated function, and either chase the wrong code or assume the doc is unreliable.
- **Suggested fix:** Replace the bare line citation with a stable anchor: `enumerate_files / enumerate_files_iter in src/lib.rs (search for "WalkBuilder::follow_links(false)")` so the doc no longer drifts every release. If a literal line number is preferred, update to `src/lib.rs:771`.

#### DOC-V1.38-5: CHANGELOG `[Unreleased]` is empty; ~14 user-visible PRs landed since v1.38.0
- **Difficulty:** easy
- **Location:** `CHANGELOG.md:8-10` — `[Unreleased]` block is a single header line followed immediately by `## [1.38.0] - 2026-05-06`. Commits between v1.38.0 cut (`4bd12645`, 2026-05-06) and HEAD include #1487 (build_compact_data tests), #1488 (daemon GC + train-data tests), #1489 (prepare_for_embedding tests), #1490 (tasklist UTF-16 BOM fix), #1491 (splade .bak rollback), #1492 (cagra .bak rollback), #1493 (drop IndexBackendError variants), #1495 (proc-macro registry), #1497 (skip-first-pass embed → schema v27), #1499 (embedding_base NULL post-skip fix), #1500 (proc-macro DX), #1501 (drop IndexBackendError wrapper), #1502 (id_map Box<str>), #1503 (reranker batch by max_length), #1504 (cmd_model_swap bail), #1505 (cqs index --model parity), #1506 (cqs ref reindex flag parity).
- **Description:** Same shape as the v1.33.0 P1-11 finding ("CHANGELOG [Unreleased] missing 10+ post-v1.33.0 PRs that landed"). Each release the [Unreleased] block goes empty at cut and stays empty for the entire next dev cycle. The bigger deal here is that #1497 bumped the schema to v27 — that's a behaviour-visible change that should land in [Unreleased] the same day the migration commit lands, not at the next release. Schema bumps in particular need to be visible to users planning upgrades, not buried in commit subjects.
- **Suggested fix:** Add an `[Unreleased]` body listing the post-v1.38.0 PRs grouped by Added / Changed / Fixed. At minimum: an "Added" line for `cqs index --model` parity (#1505), `cqs ref reindex --llm-summaries`/`--improve-docs`/`--hyde-queries` flag parity (#1506), `[derive(CqsCommands)]` proc-macro replacing `for_each_command!` (#1495); a "Changed" line for skip-first-pass embed under `--llm-summaries` (#1497, schema v27); a "Fixed" line for cagra/splade `.bak` rollback (#1491, #1492). Adopt a CI gate or pre-commit hook that fails when a non-`docs:`/`chore:` commit lands without a CHANGELOG entry.

#### DOC-V1.38-6: README "Performance" section pinned to BGE-large + v1.27.0 file; default has been EmbeddingGemma since v1.35.0
- **Difficulty:** medium
- **Location:** `README.md:950` — "Measured 2026-04-16 on the cqs codebase itself (562 files, 15,516 chunks) with CUDA GPU (NVIDIA RTX A6000, 48 GB) on WSL2 Ubuntu. Embedder: BGE-large (1024-dim). SPLADE: ensembledistil (110M, off-the-shelf). Raw measurements: [`evals/performance-v1.27.0.json`](evals/performance-v1.27.0.json)." Default preset has been `embeddinggemma-300m` since v1.35.0 (verified `src/embedder/models.rs:477` `default = true` annotation). Latency profile differs materially: 768-dim vs 1024-dim, 2K vs 512 max-seq, different ONNX session init time.
- **Description:** Stale benchmark caption — the table reports daemon/CLI numbers a user expects to reproduce on a current install, but the install ships a different default embedder than the one measured. README currently says "Embedding latency (GPU vs CPU)" gives a per-batch 50-doc CUDA cost of 0.3 ms/doc *for BGE-large*; on EmbeddingGemma-300m at 768-dim those numbers will be different. Not lying about the corpus, but lying about what the numbers describe.
- **Suggested fix:** Either (a) re-run the perf bench on the v1.38.0 default and update the table + replace `evals/performance-v1.27.0.json` with `evals/performance-v1.38.0.json`, or (b) edit the caption to "Measured 2026-04-16 on cqs v1.27.0 with the v1.27.0 default embedder (BGE-large). Numbers shift on the v1.35+ EmbeddingGemma default — re-bench planned for v1.39." Option (b) is the cheap-fix; (a) is the right-thing-to-do.

#### DOC-V1.38-7: SECURITY.md lists schema as `v22, v23, v24, v25, v26` for vendored-code chunks; v27 not enumerated
- **Difficulty:** easy
- **Location:** `SECURITY.md:59` — "(#1221, schema v24)" reference is correct, but the broader v23-v25-v26 enumeration in CONTRIBUTING.md and the lack of any v27 mention in either file means an auditor reading SECURITY.md / CONTRIBUTING.md alongside the schema header in `src/schema.sql:1` (`cq index schema v27 ... v22+v23+v24+v25+v26+v27 columns annotated inline`) sees a one-version mismatch. SECURITY.md doesn't mention v27 at all. Same root cause as DOC-V1.38-3 but on a different surface.
- **Description:** Not lying so much as silently incomplete — v27 added `chunks.needs_embedding`, which controls whether a chunk goes into the first-pass embed pool under `--llm-summaries`. That's behaviour-visible to anyone tracing a "why isn't this chunk in HNSW yet?" path. Worth one paragraph in either SECURITY.md (under "Index Storage") or PRIVACY.md (under "What Gets Stored") so post-skip chunks aren't a mystery.
- **Suggested fix:** Add a sentence to `SECURITY.md` "Index Storage" section (line 277-280): "Schema v27 (#1497) adds `chunks.needs_embedding`; chunks created during a `--llm-summaries` reindex skip the initial cold embed and get embedded once the LLM-enriched description is available, then cleared by `enrichment_pass`." Update `PRIVACY.md:15` similarly.

#### DOC-V1.38-8: Cargo.toml `Description` declares "50.9 / 76.2 / 88.6" but README leaves the bge-large/v9-200k/nomic rows pre-retune
- **Difficulty:** easy
- **Location:** `Cargo.toml:9` description: "50.9% R@1 / 76.2% R@5 / 88.6% R@20 on v3.v2 dual-judge code-search (218 queries, EmbeddingGemma-300m default with per-category SPLADE α retuned for the new dense backbone)". `README.md:704` admits "Other rows are pre-retune (apples-to-apples 2026-05-02 on cqs v1.35.0, all 5 slots reindexed `--force --llm-summaries`); their numbers will shift up under the new alphas, but a fresh sweep across all five slots is queued." Status of the queued sweep: no follow-up commit since 2026-05-03 retune; non-gemma rows in the table are still pre-retune. v1.33.0 audit P1-5/P1-6 flagged the equivalent issue and parked it as ticket #1369 (which I cannot verify is still open without GitHub access).
- **Description:** Not lying about gemma's numbers — those are real. But the README's per-preset table presents the pre-retune BGE/v9/coderank rows as if they're directly comparable to the gemma row, with a one-paragraph caveat agents are likely to skip. Prior audit triage flagged this as P1; no fix has landed.
- **Suggested fix:** Either (a) run the queued sweep across the 4 non-gemma slots and refresh rows 709-712 of `README.md`, or (b) hide the pre-retune rows behind a `<details>` block titled "Other presets (pre-retune)" so the gemma row is the unambiguous default-config number.

#### DOC-V1.38-9: README "How It Works" Step 1 says "19 other chunk types"; actual count differs and was last counted in v1.27 era
- **Difficulty:** easy
- **Location:** `README.md:670` — "Tree-sitter extracts functions, classes, structs, enums, traits, interfaces, constants, tests, endpoints, modules, and 19 other chunk types across 54 languages". Verified by counting `define_chunk_types!` rows in `src/language/mod.rs:683-744` → ~30 distinct rows (Function, Method, Class, Struct, Enum, Trait, Interface, Constant, Section, Property, Delegate, Event, Module, Macro, Object, TypeAlias, Extension, Constructor, Impl, ConfigKey, Test, Variable, Endpoint, Service, StoredProc, Extern, Namespace, Middleware, Modifier, ...). The "10 enumerated + 19 other = 29" arithmetic was right when written; current total is higher.
- **Description:** Mild drift — the kind of claim contributors are supposed to update under CONTRIBUTING.md "Adding a New Chunk Type → 5. Update docs". Not load-bearing for correctness, but agents quoting the count back at users will be off by one or two. Worth fixing in the same pass as DOC-V1.38-3 since both are "count the rows" stalenesses.
- **Suggested fix:** Replace the literal count with a generated-from-source pointer: "...constants, tests, endpoints, modules, and 20+ other chunk types across 54 languages (see `src/language/mod.rs::define_chunk_types!` for the full list)". Avoids future drift and saves the contributor checklist step.

#### DOC-V1.38-10: PROJECT-MEMORY drift in `MEMORY.md` — schema "v22", test counts, version, fixture R@K all stale
- **Difficulty:** easy
- **Location:** `~/.claude/projects/-mnt-c-Projects-cqs/memory/MEMORY.md` (loaded as user auto-memory for every session): "Version: 1.29.1", "Schema: v22", "Tests: ~1717 lib tests post-#1105", "Metrics (refreshed v3.v2, BGE-large, 2026-04-25): test R@5 63.3%, dev R@5 74.3%". Actual: version 1.38.0 (Cargo.toml:1), schema 27 (`src/store/helpers/mod.rs:151`), default model embeddinggemma-300m (not bge-large), README's measured number agg R@5 76.2% on gemma. The memory blob explicitly warns "Do NOT cite the 63.3%/74.3% as current state" but then cites v1.29.1, v22, etc. as if current.
- **Description:** Not a repo-facing doc per se, but it ships into every Claude session for this project — meaning every audit, every plan, every fresh-eyes review starts from a 9-version-stale schema number, a 9-version-stale binary version, and a model that hasn't been the default for 3 minor versions. Higher-leverage to fix than any single README typo because it biases every downstream decision the assistant makes. Out of scope for a code audit if you read "Documentation" strictly as repo-only files; calling it out here because the audit prompt explicitly mentioned "Documentation" without scoping to the repo, and the memory file is the single highest-traffic stale doc in the system.
- **Suggested fix:** User runs through MEMORY.md and replaces: `Version: 1.29.1` → `Version: 1.38.0`, `Schema: v22` → `Schema: v27`, default embedder line `BGE-large default (1024-dim)` → `EmbeddingGemma-300m default (768-dim, since v1.35.0)`, refresh test count via `cargo test --features cuda-index 2>&1 | grep "^test result:"`, drop the canonical-pre-refresh metric paragraph (it's two regression cycles old now). Adopt a `groom-memory` skill (sibling of `groom-notes`) that re-runs every minor release.

---

## Summary

10 findings, 4 of which are "lying docs" by the team's P1 rule (DOC-V1.38-1 `--improve-docs` write-back claim across README/lib.rs/SECURITY.md; DOC-V1.38-2 CONTRIBUTING.md describing a deleted file and macro; DOC-V1.38-3 schema v26 vs actual v27 in CONTRIBUTING.md; DOC-V1.38-4 stale `lib.rs:813` line citation in SECURITY.md). Two are completeness gaps that promise nothing false but leave a release of churn invisible (DOC-V1.38-5 empty `[Unreleased]`; DOC-V1.38-7 v27 schema not surfaced in SECURITY/PRIVACY). Three are stale-but-not-quite-lying (DOC-V1.38-6 perf table pinned to v1.27.0/BGE-large; DOC-V1.38-8 unrefreshed non-gemma R@K rows; DOC-V1.38-9 chunk-type count). One is project-memory drift (DOC-V1.38-10).

The cluster signal: `--improve-docs` semantics changed in v1.30.1 and three docs disagree about it three minor versions later. The repeat offender is line-number citations and version numbers — same drift pattern hit `lib.rs:601 → 813 → 771` and `Schema v25 → v26 → v27` across two consecutive audits. Mechanical fix: switch to anchor-based citations (function-name search rather than literal line) and add a CHANGELOG-presence pre-commit hook so the [Unreleased] block can't go empty for an entire dev cycle again.

---

## API Design

# API Design audit — batch 1 (post-v1.38.0)

Audit cuts: CLI argument shape consistency, trait method shape, top-level `Cli` global flags, error variant hygiene. Skips items already addressed in #1505/#1506/#1507/#1501/#1500/#1470 per task scope. All paths absolute.

## Findings

#### API-V1.38-1: `ModelCommand` and `HookCommand` use inline `json: bool` instead of shared `TextJsonArgs`
- **Difficulty:** easy
- **Location:** `/mnt/c/Projects/cqs/src/cli/commands/infra/model.rs:122-145` (`ModelCommand::{Show,List,Swap}`), `/mnt/c/Projects/cqs/src/cli/commands/infra/hook.rs:55-97` (`HookCommand::{Install,Uninstall,Fire,Status}`)
- **Description:** Project / Ref / Slot / Cache / Notes / Init / Doctor / Stats / Affected / Brief / Refresh / Ping / Status / Convert all flatten `TextJsonArgs` (audit IDs API-V1.22-2, API-V1.29-1/2, P3-25/26/30). Model and Hook are the holdouts and still ship inline `json: bool` per-variant. Two consequences: (1) every new shared output knob (e.g. `--pretty`, `--ndjson`) becomes a multi-file edit instead of one-line, (2) the top-level `cqs --json model show` propagation has to be hand-merged in the dispatcher (`cli.json || *json` per arm) instead of riding the shared resolver.
- **Suggested fix:** Replace the inline `json: bool` fields on every variant of `ModelCommand` and `HookCommand` with `#[command(flatten)] output: TextJsonArgs`, then collapse the per-arm `cli.json || *json` to read `cli.json || output.json`. Non-breaking — the user-facing `--json` flag stays.
- **Tag:** non-breaking

#### API-V1.38-2: `ProjectCommand::Search` duplicates `query/limit/threshold` instead of flattening `SearchArgs`
- **Difficulty:** medium
- **Location:** `/mnt/c/Projects/cqs/src/cli/commands/infra/project.rs:85-97`
- **Description:** `ProjectCommand::Search` defines its own bare `query: String`, `limit: usize`, `threshold: f32` fields. CQ-V1.25-1/4 already extracted `SearchArgs` as the single source of truth for every search knob (21 fields: `--rrf`, `--name-boost`, `--include-type`, `--exclude-type`, `--pattern`, `--include-docs`, `--reranker`, `--splade*`, `--expand-parent`, `--no-demote`, `--no-stale-check`, `--lang`, `--path`). Cross-project search silently drops every one of those — agents who learn `cqs scout foo --reranker onnx` and reach for `cqs project search foo --reranker onnx` get an "unexpected argument" error, with no signal that cross-project search is a different surface. Item 2 of #1459 ("project / ref verb consolidation") is the umbrella but the field-level duplication is the concrete bug. **Bonus**: `threshold` here lacks `value_parser = parse_finite_f32`, so NaN/Inf bypass the validator that protects every other threshold flag (AC-V1.29-5).
- **Suggested fix:** Replace inline fields with `#[command(flatten)] args: SearchArgs` (the same pattern `Commands::Search { args }` already uses). Pipe `args` through `search_across_projects` so cross-project search honors filters, name boost, RRF, reranker, etc. If full parity is too risky in one PR, at minimum (a) wire `value_parser = parse_finite_f32` on `threshold`, (b) add the missing `--lang` / `--include-type` / `--exclude-type` / `--rrf` / `--reranker` knobs as a quick-fix.
- **Tag:** non-breaking (additive flags); breaking if shared `SearchArgs` semantics differ at handler

#### API-V1.38-3: `BatchProvider` trait lacks `Send + Sync` bounds while sibling traits require them
- **Difficulty:** easy
- **Location:** `/mnt/c/Projects/cqs/src/llm/provider.rs:115`
- **Description:** `pub trait BatchProvider {` has no auto-trait bounds, but every sibling trait does: `VectorIndex: Send + Sync` (`/mnt/c/Projects/cqs/src/index.rs:32`), `IndexBackend: Send + Sync` (`index.rs:127`), `Reranker: Send + Sync` (`reranker.rs:187`). The result is asymmetric: `Arc<dyn Reranker>` can move across threads, `Box<dyn BatchProvider>` (the actual return type from `create_client`, see `llm/mod.rs:415`) can't. The Anthropic provider is fine across threads (HTTP client is `Send + Sync`); the Local provider's worker pool fan-out already implies `Send + Sync` at the impl level. Today nothing tries to hold the trait object across an `await` or `spawn`, but the next async-batch / parallel-validation refactor will — and the failure will be a confusing object-safety error rather than an explicit compiler complaint.
- **Suggested fix:** `pub trait BatchProvider: Send + Sync {`. Both shipping impls (`LlmClient` in `llm/batch.rs` and `LocalProvider` in `llm/local.rs`) already satisfy it; `MockBatchProvider` in `llm/provider.rs:186` is `#[cfg(test)]` and trivially `Send + Sync`.
- **Tag:** non-breaking

#### API-V1.38-4: `LocalProvider::submit_batch` skips `validate_model` despite the trait contract
- **Difficulty:** easy
- **Location:** `/mnt/c/Projects/cqs/src/llm/local.rs:830-860` vs trait doc at `/mnt/c/Projects/cqs/src/llm/provider.rs:160-164`
- **Description:** `BatchProvider::validate_model`'s docstring (EXT-V1.36-1 / P3) says it "is called from `submit_batch` *before* the API roundtrip so a wrong-provider/model combo (e.g. `--provider anthropic --model gpt-4o`) fails fast with the offending name in the error instead of surfacing as an opaque API error." `LlmClient::submit_batch` (`llm/batch.rs:362`) honors this: `self.validate_model(&self.llm_config.model)?;` is the first line. `LocalProvider::submit_batch` doesn't call it at all — the dispatch table goes straight into `submit_via_chat_completions`. A future provider-validation tightening (e.g. local provider rejecting empty model name) will silently fall on the floor for every Local user.
- **Suggested fix:** Either (a) add `self.validate_model(&self.config.model)?;` as the first line of `LocalProvider::submit_batch`, or (b) hoist the `validate_model` call into a default `submit_batch_validated` template method in the trait that impls override only for the actual submission, removing the foot-gun entirely.
- **Tag:** non-breaking

#### API-V1.38-5: `Cli::resolved_model` is `pub` instead of `pub(super)`
- **Difficulty:** easy
- **Location:** `/mnt/c/Projects/cqs/src/cli/definitions.rs:314-315`
- **Description:** The field is `#[arg(skip)] pub resolved_model: Option<cqs::embedder::ModelConfig>` — set by `dispatch::resolve_model`, read by handlers via `cli.try_model_config()`. The `pub` visibility is broader than necessary: every other `Cli` field is `pub` because `clap::Parser` requires it, but `resolved_model` is `#[arg(skip)]` and nothing outside this binary crate touches it. The comment "set by dispatch, not CLI" implies the author knew it shouldn't be poked externally; the type system isn't enforcing that. (Lower-stakes than the inline-`json` arms because `Cli` itself is binary-only.)
- **Suggested fix:** Change to `pub(super) resolved_model: Option<...>` so only `cli/dispatch.rs` and `cli/definitions.rs` can write it; readers go through `try_model_config()` which is already `pub`. Confirm `cli/dispatch.rs` is in the same `super` (it is — `crate::cli`).
- **Tag:** non-breaking

#### API-V1.38-6: Top-level `Cli` search flags silently ignored when subcommand is given
- **Difficulty:** medium
- **Location:** `/mnt/c/Projects/cqs/src/cli/definitions.rs:155-289` (limit/threshold/name_boost/lang/include_type/exclude_type/path/pattern/name_only/rrf/include_docs/reranker/splade/splade_alpha/expand_parent/ref_name/include_refs/tokens/no_stale_check/no_demote/model)
- **Description:** Top-level Cli has 20+ search-shaped flags that exist for the bare `cqs <query>` shorthand. They're NOT marked `global = true` (only `--slot` is). Two failure modes: (1) `cqs scout foo --rrf` → clap rejects "unexpected argument" because `ScoutArgs` doesn't carry `--rrf`. (2) `cqs --rrf scout foo` → parses successfully, scout's handler doesn't read `cli.rrf`, the flag is silently dropped. I just verified mode (2): scout's `cmd_scout` (`cli/commands/search/scout.rs:42`) takes `task / limit / json / max_tokens` only — every other top-level search flag is dead air. Multiple commands have the same shape: `gather`, `where`, `task`, `plan`, `onboard`, `related` all have their own `*Args` structs that exclude `--rrf`, `--name-boost`, `--include-docs`, `--reranker`, etc. An agent who learns the bare-query flags can't transfer them to any command that wraps a search.
- **Suggested fix:** Two options. **(a)** Mark search-shaping flags `global = true` and have all search-wrapping commands' inner `cmd_*` read them from `cli` instead of args (this matches how `--slot` works today). **(b)** Promote the `--rrf` / `--include-docs` / `--reranker` / `--name-boost` / `--no-demote` knobs into a shared `SearchKnobsArgs` struct and flatten it into `ScoutArgs / GatherArgs / WhereArgs / TaskArgs / PlanArgs / OnboardArgs / RelatedArgs`. Path (b) is the cleaner long-term shape but bigger surface; (a) closes the silent-drop today. Even just an explicit warn-on-ignored at handler boundary would prevent silent-drop.
- **Tag:** non-breaking (additive)

#### API-V1.38-7: `ExportModel.dim` is `Option<u64>` while every other dim field is `usize`
- **Difficulty:** easy
- **Location:** `/mnt/c/Projects/cqs/src/cli/definitions.rs:781-782`
- **Description:** `ExportModel { dim: Option<u64>, ... }` — every other place dim is plumbed it's `usize`: `ModelConfig.dim: usize` (`embedder/models.rs:157`), `EmbeddingConfig.dim: Option<usize>` (`models.rs:863`), `VectorIndex::dim() -> usize`, `Embedding::len() -> usize`. The `u64` here serves no purpose — embedding dim is a small integer (768/1024 today, not even close to `u32::MAX`) and the export-model command writes it into a TOML file as a string. Passing through `as usize` at the handler boundary obscures the fact that the rest of the codebase agrees on the type.
- **Suggested fix:** Change `dim: Option<u64>` → `dim: Option<usize>` and drop the `as usize` casts at the handler. Compatible — `--dim 768` parses identically.
- **Tag:** non-breaking

#### API-V1.38-8: Two flags for the same semantic — `--wait-secs` (status) vs `--require-fresh-secs` (eval)
- **Difficulty:** easy
- **Location:** `/mnt/c/Projects/cqs/src/cli/definitions.rs:892-895` (`Status.wait_secs`), `/mnt/c/Projects/cqs/src/cli/commands/eval/mod.rs:94-95` (`EvalCmdArgs.require_fresh_secs`), comment at `definitions.rs:889-891` already notes the duplication
- **Description:** Both flags wait for `cqs watch --serve` to report `state == fresh` and bound the wait. `cqs status --wait-secs 30` and `cqs eval --require-fresh-secs 600` use different spellings for the same semantic. The comment "Note: `cqs eval --require-fresh-secs` has the same semantics; default differs by use case" acknowledges this — but two flag names is exactly the muscle-memory cost #1459 / `--depth` vs `--commits` vs `--max-depth` was filed to clean up. Agents that learn one don't transfer to the other.
- **Suggested fix:** Pick one canonical spelling (`--wait-secs` is shorter and matches the `cqs status --wait` semantic of "block until ready"; `--require-fresh-secs` is more self-documenting in CI logs). Add the other as a `visible_alias` so existing scripts keep working. Defaults can stay command-specific (30 for status, 600 for eval).
- **Tag:** non-breaking (with alias)

#### API-V1.38-9: `EmbedderError::HfHub(String)` and `RerankerError::ModelDownload(String)` are sibling stringified errors
- **Difficulty:** medium
- **Location:** `/mnt/c/Projects/cqs/src/embedder/mod.rs:56-57`, `/mnt/c/Projects/cqs/src/reranker.rs:142-143`
- **Description:** Two error variants in two separate enums both wrap a String description of "the HuggingFace fetch failed". Both flow through `aux_model::resolve` (the shared resolver). The reranker variant is named `ModelDownload`, the embedder variant `HfHub` — same root cause, different display string ("Model download failed: …" vs "HuggingFace Hub error: …"), different name. Keeps the two error pipelines from sharing rendering / retry logic. Smaller pain than the API-V1.36 `IndexBackendError` collapse but the same shape.
- **Suggested fix:** Promote `aux_model::ResolveError` (which already exists internally per the `.map_err` calls) to a `pub` shared error and have both `EmbedderError` and `RerankerError` `#[from]` it: `#[error(transparent)] AuxModel(#[from] crate::aux_model::ResolveError)`. Both display strings collapse to the inner error's. Drop both stringly-typed variants.
- **Tag:** breaking (variant rename, but `pub use` aliases can preserve the name in display strings)

#### API-V1.38-10: `LimitArg` flattened in 6 places, but inline `limit: usize` still exists in 7 sister args
- **Difficulty:** easy
- **Location:** Inline: `SearchArgs.limit` (`args.rs:122`), `GatherArgs.limit` (`args.rs:271`), `ScoutArgs.limit` (`args.rs:315`), `RelatedArgs.limit` (`args.rs:473`), `WhereArgs.limit` (`args.rs:521`), `PlanArgs.limit` (`args.rs:530`), `TaskArgs.limit` (`args.rs:546`). Flattened: `ImpactArgs / TraceArgs / OnboardArgs / ExplainArgs / TestMapArgs / DepsArgs / CallersArgs` — all use `#[command(flatten)] limit_arg: LimitArg`.
- **Description:** Task A3 (`args.rs:101-106`) defined `LimitArg` to "standardise `--limit` across every graph subcommand" but stopped at the graph commands. The 7 search-shaped commands still inline `#[arg(short = 'n', long, default_value = "5")] pub limit: usize`. The default is consistent (5 across all), the field is identical — but a future change to the cap (e.g. adding `value_parser` to reject `0`, or bumping default to 10) needs a 7-file edit instead of one-line. **Concrete bug**: only `SearchArgs.limit` would benefit from a `value_parser = parse_nonzero_usize` (search with `--limit 0` is meaningless), but adding it requires editing all 7 inline copies.
- **Suggested fix:** Replace inline `limit: usize` in the 7 search-shaped args with `#[command(flatten)] limit_arg: LimitArg`, update handlers from `args.limit` → `args.limit_arg.limit`. While there, add `value_parser = parse_nonzero_usize` to `LimitArg.limit` so `--limit 0` is rejected at parse time across all 13 commands at once.
- **Tag:** non-breaking (handler-internal field rename)

## Summary

API-V1.38-1 (model/hook envelope) and API-V1.38-10 (LimitArg fan-out) are pure cleanup: low risk, immediately delete code, complete patterns that are already 80% rolled out. API-V1.38-2 (ProjectCommand::Search) and API-V1.38-6 (top-level search-flag ignore) are the highest-impact items — both surface the gap between "agent learns one search command" and "agent learns N near-twins"; both close real silent-drop / parse-rejection failure modes. API-V1.38-3 (BatchProvider Send+Sync) and API-V1.38-4 (validate_model) are trait-shape hygiene with one-line fixes; today nothing depends on them but the next async refactor will. API-V1.38-7 (`u64` dim) and API-V1.38-5 (`pub` resolved_model) are scope-tightening with no behavior change. API-V1.38-8 (`--wait-secs` vs `--require-fresh-secs`) is a renamed-flag muscle-memory item, alias-friendly. API-V1.38-9 (HF error variants) is medium-effort but matches the IndexBackendError collapse pattern that #1501 just landed — same architectural argument applies.

Most are **non-breaking**; only API-V1.38-9 is breaking (variant rename), and even that can be aliased. Stable order if doing a single PR: 1, 10, 7, 5, 3, 4, 8, 6, 2, 9.

---

## Error Handling

## Error Handling

Audit pass against the post-v1.38.0 main branch (4a31285e). EH-V1.36-* items
from PR #1456 are excluded; the destructive `cmd_model_swap` migration to
`try_stored_model_name` (PR #1504) is excluded. Findings below cover
remaining call sites that still use the lossy variant in destructive /
data-correctness paths, plus a handful of new silent-error sites in
recently-touched modules.

#### EH-V1.38-1: `watch/mod.rs:1037` resolves embedding model via lossy `stored_model_name()` — silent metadata-read failure → wrong dim → corrupted incremental reindex
- **Difficulty:** medium
- **Location:** `src/cli/watch/mod.rs:1037`
- **Description:** `let stored_model_for_watch = store.stored_model_name();` is
  the watch-loop's index-aware model resolver — its result feeds
  `ModelConfig::resolve_for_query` so the daemon embeds new chunks with the
  same model that built the index. Per the EH-V1.36-6 finding the unfixed
  `stored_model_name()` returns `None` on **any** SQL error (corrupt
  metadata table, sqlite I/O error, schema mismatch). When that happens the
  resolver silently falls through to CLI flag → env → config → default —
  exactly the corrupting-incremental-reindex footgun the fix in PR #1504
  was meant to close. The comment block at lines 1032-1036 explicitly
  states the consequence ("would embed new chunks with a different dim
  than the index, corrupting incremental reindex") yet the call still
  uses the swallowing variant.
- **Suggested fix:** `let stored_model_for_watch = match store.try_stored_model_name() { Ok(s) => s, Err(e) => { tracing::error!(error = %e, "watch: failed to read stored_model_name; refusing to start to avoid mixed-dim writes"); return Err(e.into()); } };` Watch is a long-running daemon — bail rather than silently degrade.

#### EH-V1.38-2: `watch/rebuild.rs:187` same pattern in `resolve_index_aware_model_for_watch`
- **Difficulty:** medium
- **Location:** `src/cli/watch/rebuild.rs:187`
- **Description:** The companion helper used during daemon-thread bring-up
  (`daemon_model_config`) also calls `s.stored_model_name()` (lossy) inside
  an `Ok(s) =>` arm. The `Err(e)` arm (open-readonly failed) does warn,
  but the inner call returns silently on metadata read failure. Same
  consequence as EH-V1.38-1: dim drift in the daemon's reindex path
  without observability.
- **Suggested fix:** Replace the inner `s.stored_model_name()` with
  `s.try_stored_model_name()` and surface the error via `tracing::warn!`
  with `path = %index_path.display()` before falling back to None.

#### EH-V1.38-3: `model.rs:240` `cmd_model_show` reports "<unrecorded>" for both fresh-DB and corrupt-DB cases
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/model.rs:239-241`
- **Description:** `let model = store.stored_model_name().unwrap_or_else(|| "<unrecorded>".to_string());` — `cqs model show` is the operator's first-line diagnostic when troubleshooting model-mismatch errors. If the metadata read fails (corrupt DB, schema skew), the user sees "<unrecorded>" identical to a fresh DB and concludes "I haven't indexed yet" — when actually the DB is broken and the next `cqs index` call without `--force` will hit the EH-V1.38-1 path. The strict variant exists; show is the diagnostic surface that most needs it.
- **Suggested fix:** Branch on the `Result`: emit `<unrecorded>` only on `Ok(None)`; on `Err(e)` print a distinct `<read-error: {e}>` and add a warn line so the user can `cqs doctor` from there.

#### EH-V1.38-4: `slot.rs:206` slot listing silently empties model column on metadata read failure
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/slot.rs:206`
- **Description:** `cqs slot list` displays `model` per slot. When the
  metadata read fails (e.g., a slot's index is mid-rebuild and metadata
  is locked), the column shows empty — same display as a fresh slot.
  Operators auditing which slots use which model can't tell broken from
  fresh. Note the surrounding code at lines 210-218 already has a warn
  ladder for the **store open** failure case; the metadata read inside
  `Ok(store) =>` is the only branch missing observability.
- **Suggested fix:** Replace `let model = store.stored_model_name();` with
  `let model = match store.try_stored_model_name() { Ok(m) => m, Err(e) => { tracing::warn!(slot = name, error = %e, "Failed to read model_name from slot metadata"); None } };`

#### EH-V1.38-5: `cli/dispatch.rs:139-142` slot SPLADE α resolver still uses `.ok()` despite EH-V1.30.1-3 fixing the same pattern 30 lines above
- **Difficulty:** easy
- **Location:** `src/cli/dispatch.rs:139-142`
- **Description:** Lines 107-119 of the same function fixed an identical
  silent-suppression bug (EH-V1.30.1-3 cited in the comment) by
  converting `resolve_slot_name(...).ok()` into a `match` with
  `tracing::warn!`. Lines 139-142 then immediately re-introduce the same
  `.ok()` pattern for the SPLADE α resolution: `cqs::slot::resolve_slot_name(...).ok().map(...).unwrap_or_default()`. If the user passes `--slot foo` and resolution fails (typo, missing slot file), they silently get default-slot α overrides — the only signal being *different search results from what they asked for*. The fix is the exact one already applied above.
- **Suggested fix:** Replicate the match-with-warn pattern from lines
  107-119; on Err, emit a warn citing `slot = ?cli.slot` and fall through
  to the empty alpha table.

#### EH-V1.38-6: `cli/commands/infra/hook.rs:393, 536` conflate `NotFound` with permission-denied / oversize errors
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/hook.rs:393` and `src/cli/commands/infra/hook.rs:536`
- **Description:** `match read_hook_capped(&path) { Err(_) => report.not_present.push(...), ... }` lumps three distinct conditions: (a) hook actually missing (expected, fine), (b) hook present but permission-denied or other I/O error, (c) hook present but exceeds the 1 MiB cap (already logged as warn inside `read_hook_capped` but then gets silently downgraded to "not present" in the report). On `cqs hook status`, the operator sees "missing" and runs `cqs hook install`, which then unconditionally writes to the path — silently overwriting whatever oversize/perm-locked file is there. Compare with the inline `warn` already in `read_hook_capped` for the cap case — that warn fires but the caller's `Err(_)` arm reports "missing" anyway, contradicting the warn.
- **Suggested fix:** Match on `e.kind()`: `ErrorKind::NotFound => report.not_present.push(...)`; everything else into a new `report.unreadable: Vec<(String, String)>` (or push to a third bucket) and surface in the output. At minimum, `tracing::warn!(hook, error = %e, "Hook present but unreadable; treating as not-present")` so the operator sees the discrepancy.

#### EH-V1.38-7: `embedder/mod.rs:1325-1334` triple-cascade tensor extract silently swallows f32 and f16 extract errors
- **Difficulty:** medium
- **Location:** `src/embedder/mod.rs:1325-1334`
- **Description:** The dtype probing chain `try_extract_tensor::<f32>() → ::<f16>() → ::<bf16>()` uses bare `Err(_) =>` for the first two fallbacks. ORT's extract errors include both the expected "wrong dtype" (innocuous, the next branch handles it) AND real failures like "session output index out of range" or "tensor backing memory invalid". When a non-dtype error fires for f32, we silently try f16, get a different error there, silently try bf16, and finally surface only the bf16 error via `ort_err`. The operator sees a confusing "bf16 extract failed" message while the actual problem was the f32 extract — wrong root cause in the log. Hot path; this is the cqs query-time inference loop.
- **Suggested fix:** Distinguish "wrong dtype" from "real error" via the `OrtError` variant. Or simpler: probe the dtype once via `output.dtype()` and dispatch directly — no cascade needed. Each branch knows up front which extract to call.

#### EH-V1.38-8: `parser/{calls,injection,aspx}` log tree-sitter `LanguageError` via `error = ?e` (Debug) instead of `error = %e` (Display)
- **Difficulty:** easy
- **Location:** `src/parser/calls.rs:53`, `src/parser/injection.rs:278`, `src/parser/aspx.rs:236`
- **Description:** Three sites log a tree-sitter `LanguageError` (returned by `parser.set_language`) using `error = ?e`. The neighboring code in the same files (e.g. `aspx.rs:241`, `injection.rs:287`) uses `%e` for the next call's `IncludedRangesError` — inconsistent within the file. tree-sitter errors implement `Display`; the `?e` form expands to multi-line Debug output (`LanguageError { ... }`) instead of a one-line summary. Audit-finding type EH-V1.36 already established the project's preference for `%`; these three sites missed the bus.
- **Suggested fix:** Replace `error = ?e` with `error = %e` at all three sites. Same change in `cagra.rs:268` if `CagraError` impls Display (it does, via `thiserror`).

#### EH-V1.38-9: `cli/json_envelope.rs:371` and `cli/batch/mod.rs:2235` discard original `to_string_pretty` / `to_writer` error in the sanitize-and-retry path
- **Difficulty:** easy
- **Location:** `src/cli/json_envelope.rs:368-377`, `src/cli/batch/mod.rs:2235-2257`
- **Description:** Both sites do `Ok(s) => Ok(s), Err(_) => sanitize-and-retry`. The comment at the call sites says "NaN / Infinity caused this", but `to_writer`/`to_string_pretty` can fail for other reasons (downstream `io::Write` error in the batch case; serde custom Serialize error; recursion limits). When one of those non-NaN errors fires, the sanitize-retry path produces a structurally identical Value, the second serialize fails the same way, and `tracing::warn!` at line 2247 logs only the *second* (post-sanitize) error — not the original. Operator sees a misleading "JSON serialization failed after sanitization" when the real cause was the I/O error on the first attempt.
- **Suggested fix:** Capture the first error: `Err(e) => { let first = e; tracing::debug!(error = %first, "to_writer failed; retrying after float-sanitize"); ... }`. If the second attempt also fails, include both in the warn. Cheap and the I/O-error case becomes diagnosable.

#### EH-V1.38-10: `cli/limits.rs:207-223` `parse_env_usize` / `parse_env_u64` silently accept malformed values when env var is set
- **Difficulty:** easy
- **Location:** `src/cli/limits.rs:207-223`
- **Description:** `parse_env_usize`: `v.parse::<usize>().ok().filter(|n| *n > 0).unwrap_or(default)`. If the user sets `CQS_RERANKER_POOL_SIZE=abc` or `=0`, the env var is silently ignored and the default is used — no warn. Compare with `pipeline/types.rs:99-111`, `gather.rs:155-177`, `trace.rs:355-375`, and dozens of other env-knob helpers in this repo, all of which `tracing::warn!(value = %val, "Invalid X, using default Y")` in the malformed-but-set case. `parse_env_usize`/`parse_env_u64` are unique outliers — and they back at least 6 production knobs (`rerank_pool_size`, `rerank_max_batch_size`, etc.). An operator setting `CQS_RERANK_POOL_SIZE=128 ` (trailing space, copy-paste from a YAML file) gets the default with no signal.
- **Suggested fix:** Add a warn to both helpers: `if !v.is_empty() && v.parse::<usize>().ok().filter(|n| *n > 0).is_none() { tracing::warn!(env = key, value = %v, "Invalid env var value, using default {default}"); }` Mirrors every other env-knob helper in the repo.

---

## Observability

# Observability Audit — post-v1.38.0

Scope: code merged after v1.38.0 release through commit `fe7126c4` (PR #1506). Prior obs cleanup landed in #1208/#1362 (v1.33.0); this pass focuses on additions in PRs #1466..#1506 plus a few latent gaps the new code re-exposed.

---

#### OB-V1.38-1: `cmd_ref_update` lacks an entry span — entire ref reindex path invisible in tracing
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/reference.rs:599`
- **Description:** PR #1506 grew `cmd_ref_update` into a full LLM/HyDE/doc-comment/enrichment pipeline (~150 added lines, parity with `cmd_index`). The parent `cmd_ref` at line 130 has `tracing::info_span!("cmd_ref")`, but the per-subcommand handler `cmd_ref_update(cli, name, json, opts)` carries no span of its own. `cmd_ref_add`, `cmd_ref_list`, `cmd_ref_remove` are also span-less. Operators correlating "why did the ref reindex pipeline take 18 minutes?" against journal logs see only the `cmd_ref` parent — they can't tell which subcommand ran, which ref name was targeted, or which post-pipeline pass (LLM / HyDE / docs / enrichment) is the slow leg without RUST_LOG=debug per-pass tracing.
- **Suggested fix:** Add `let _span = tracing::info_span!("cmd_ref_update", ref_name = name, llm_summaries = opts.llm_summaries, improve_docs = opts.improve_docs, hyde_queries = opts.hyde_queries).entered();` at line 612 (after the flag-invariant bail). Mirror in `cmd_ref_add` / `cmd_ref_remove` / `cmd_ref_list` for consistent surface coverage.

#### OB-V1.38-2: `check_index_model_drift` bails silently — no `tracing::warn!` accompanies the fatal error
- **Difficulty:** easy
- **Location:** `src/cli/commands/index/build.rs:1496-1525` (call site at `:357`)
- **Description:** PR #1505's drift-detection footgun fix (`cqs index --model X` against a Y-built store) is the new actionable failure mode: `anyhow::bail!` returns a long help string. But it never emits a `tracing::warn!` or `tracing::error!`, so when the daemon-spawned `cqs index` is invoked from a hook or via the daemon and the operator only has journald logs, the structured event "index drift detected, refusing to clobber" never lands. The bail message goes to stderr only — daemon-mode loses it.
- **Suggested fix:** Before `anyhow::bail!` at line 1518, add `tracing::warn!(stored_model = %stored_name, requested_model = %requested_name, index_path = %index_path.display(), "Index model drift detected — refusing to clobber, exiting");`. Operators harvesting journald can then alert on this condition without parsing stderr.

#### OB-V1.38-3: synonym/classifier overlay install is silent — operators can't see whether their TOML config was applied
- **Difficulty:** easy
- **Location:** `src/search/synonyms.rs:81` (install_synonym_overlay), `src/search/router.rs:519` (install_classifier_vocab_overlay), `src/search/router.rs:689` (install_slot_splade_alpha_overrides), all called from `src/cli/dispatch.rs:143/165/185`
- **Description:** PRs #1472, #1482, #1483 added three TOML overlay surfaces (per-slot SPLADE α, synonym table, classifier vocab). Each `install_*` runs once at dispatch entry. None of them log at info level on success — `load_synonym_overlay` does emit `tracing::debug!(entries, "Loaded synonym overlay")` but at debug, and the classifier vocab loader has no completion event at all. An operator who edits `~/.config/cqs/synonyms.toml` cannot verify from journald whether the file was loaded, parsed, or applied without enabling RUST_LOG=debug. Per CLAUDE.md memory ("user-actionable info hidden behind RUST_LOG=debug" pattern), this should be info.
- **Suggested fix:** Promote `tracing::debug!` at `synonyms.rs:197` to `tracing::info!`. Add equivalent info-level events at `install_synonym_overlay` (count of merged entries), `install_classifier_vocab_overlay` (count of negation/multistep additions), and `install_slot_splade_alpha_overrides` (count of category overrides) — fire only when the input is non-empty, so the default no-overlay case stays silent.

#### OB-V1.38-4: `wait_for_batch` polling loop has no entry span and no closing elapsed_ms — Anthropic Batches API latency invisible
- **Difficulty:** easy
- **Location:** `src/llm/batch.rs:147`
- **Description:** `LlmClient::wait_for_batch` polls the Batches API every `BATCH_POLL_INTERVAL` until the batch reports `ended` / `canceling` / `canceled` / `expired`. A typical Claude batch takes 10-30 minutes; the only structured event for this entire wait is `tracing::info!(batch_id, "Batch complete")` on the success arm at line 183. There is no entry span, no elapsed_ms on the completion event, no warn/error event for the canceling/canceled/expired arms (they return `Err(LlmError::BatchFailed(...))` with no log line). Operators investigating "why did the LLM summary pass take 47 minutes?" cannot distinguish API slowness from local processing without timing the wait themselves. The `submit_or_resume` parent span (line 487) covers this transitively, but the leaf span where the actual wall-clock waiting happens emits nothing structured.
- **Suggested fix:** (1) Add `let _span = tracing::info_span!("wait_for_batch", batch_id).entered();` and `let started = std::time::Instant::now();` at the top. (2) On the `ended` arm, change the info to `tracing::info!(batch_id, elapsed_ms = started.elapsed().as_millis() as u64, "Batch complete")`. (3) On the canceling/canceled/expired arm, fire `tracing::warn!(batch_id, status = %batch.processing_status, elapsed_ms, "Batch ended in non-success state")` before returning `Err`.

#### OB-V1.38-5: CAGRA "ineligible — falling through" is at debug — operators can't see the GPU-fallback decision in journald
- **Difficulty:** easy
- **Location:** `src/cagra.rs:1530-1536`
- **Description:** When `CagraIndex::resolve_for_search` decides not to use CAGRA (chunk count below threshold OR GPU unavailable), it logs at `tracing::debug!`. The CAGRA-rebuilt and CAGRA-loaded paths (`tracing::info!(backend = "cagra", source = ..., "Vector index backend selected")` at lines 1552 and 1578) are info-level. The HNSW-fallback path is debug — so a query that should have been on GPU but silently fell back to HNSW (e.g. CUDA was momentarily unavailable, or chunk count fell below threshold after a `cqs gc`) leaves no info-level trace. Per the audit prompt: "Fallback paths (e.g., GPU unavailable → CPU) — should fire `tracing::info!` once so operators know they're on the slow path."
- **Suggested fix:** Promote the fall-through log to `tracing::info!` and tag with the same `backend = "hnsw", source = "cagra-ineligible"` shape used by the success arms so log filters on `backend` work uniformly: `tracing::info!(backend = "hnsw", source = "cagra-ineligible", chunk_count, cagra_threshold, dim, gpu_available, "Vector index backend selected")`. One line per Store init — not per-query, no spam.

#### OB-V1.38-6: skip-first-pass-embed sentinel batch logs at debug — operator running `cqs index --llm-summaries` can't see why search returns zero hits mid-run
- **Difficulty:** easy
- **Location:** `src/cli/pipeline/embedding.rs:297-301`
- **Description:** PR #1497 introduced the skip-first-pass optimisation: when `--llm-summaries` is set, the first embedding pass emits zero-vec sentinels (`needs_embedding=1`) and defers real embedding to the post-summary `enrichment_pass`. Mid-run, the index is in a state where ~50% of chunks have zero vectors and are explicitly excluded from search. The only structured event for this hot loop is `tracing::debug!(cache_hits, stamped_unembedded, "skip-first-pass: emitted zero-vec batch")` — fires per batch, but at debug. An operator searching while a `cqs index --llm-summaries` is half-way through and getting unexpectedly poor recall has no info-level signal that the index is in a transient zero-vec state.
- **Suggested fix:** Emit a single info-level event at the start of the index run (in `cmd_index` where `skip_first_pass_embed` is decided) saying "skip-first-pass enabled — chunks awaiting summary will land vectors during enrichment pass." Keep the per-batch debug log unchanged. This gives operators one stable journal line they can correlate against "search went weird at 14:32" without re-instrumenting at debug.

#### OB-V1.38-7: SPLADE save .bak rollback restore-failure is `tracing::error!` but the error path bails without surfacing to operator console
- **Difficulty:** medium
- **Location:** `src/splade/index.rs:524-545`
- **Description:** PR #1491 added `.bak` rollback: on atomic_replace failure, restore `.bak` → live name. If that restore *also* fails the error path emits `tracing::error!` and returns `SpladeIndexPersistError::Io`. The error message is great ("manual recovery — rename {bak} to {path}") but it's only emitted via `tracing::error!`. In daemon mode this lands in journald, but in interactive `cqs index` mode the rolled-up `Result<()>` propagates up as a generic `anyhow::Error` and the operator sees a one-line message — they have no way to know they need to manually rename `.bak` back unless they happen to scrollback through stderr. The same shape exists for the stale-`.bak` guard at line 470: returns Err with the recovery hint embedded but no structured tracing event records that the index was refused.
- **Suggested fix:** At line 470 (stale-`.bak` guard), emit `tracing::warn!(bak_path = %bak_path.display(), live_path = %path.display(), "SPLADE save refused — stale .bak from prior failed save, manual recovery required")` before the early return. Already-present `tracing::error!` at 524 is fine as-is — but consider re-emitting it to stderr (via `eprintln!`) when running outside the daemon context so the operator definitely sees it.

#### OB-V1.38-8: `tracing::warn!("--rerank is not supported with multi-index search...")` lacks `multi_index_count` field
- **Difficulty:** easy
- **Location:** `src/cli/commands/search/query.rs:527`
- **Description:** This warn fires when a user combines `--reranker llm` (or `bge`) with multi-index search. Single-line warn with no fields — operators can't see how many indexes were loaded, which one was active, or what the rerank flag value was. With agents running thousands of searches, when this warn fires a few times the journal entry doesn't carry enough context to reproduce the misuse.
- **Suggested fix:** `tracing::warn!(reranker = ?reranker_kind, index_count, "--rerank is not supported with multi-index search, skipping re-ranking");` so the journal line names the reranker and the multi-index breadth. Helps operators decide whether to pin to a single index or drop the rerank flag.

#### OB-V1.38-9: dispatch entry's three overlay loads have no top-level span — config-load layer is opaque in tracing
- **Difficulty:** easy
- **Location:** `src/cli/dispatch.rs:143-186`
- **Description:** Three discrete overlay-install blocks run unconditionally at dispatch entry: per-slot SPLADE α, synonym table, classifier vocab. Each block calls a `load_*_overlay` function which may hit two filesystem paths (user-global + project-local), parse TOML, validate, merge. None of these is wrapped in a `tracing::debug_span!` — so a slow disk or a wedged TOML parse appears in tracing output as unattributed time between `cqs` root span entry and the first command-level span. With no span, structured-trace consumers can't aggregate "time spent in overlay loading" across runs.
- **Suggested fix:** Wrap the three blocks (and the slot-α block before them) in a single `let _ = tracing::debug_span!("config_overlays_load").entered();` covering lines 138-186. Cheap, makes the dispatch-entry latency attributable.

#### OB-V1.38-10: index pipeline's UMAP step warn drops `chunk_count` and `dim` — debugging UMAP failures is guesswork
- **Difficulty:** easy
- **Location:** `src/cli/commands/index/build.rs:983`
- **Description:** `tracing::warn!(error = %e, "UMAP projection failed — cluster view will be unavailable")` — error field is good, but UMAP failure modes correlate strongly with chunk count (too few rows = degenerate manifold) and dimension (fp16/fp32 type mismatch on GPU). Without those fields the journal entry says "UMAP failed" with no actionable diagnostic.
- **Suggested fix:** `tracing::warn!(error = %e, chunk_count, dim, "UMAP projection failed — cluster view will be unavailable")`. `chunk_count` is in scope at the call site; `dim` is on `store.dim()` and is cheap to read.

---

## Summary

10 findings, all easy except OB-V1.38-7 (medium). Themes:

1. **New code in #1505/#1506 missed entry spans** (OB-V1.38-1, OB-V1.38-2). The post-v1.38 LLM/HyDE/doc-comment parity arm in `cmd_ref_update` is span-less, and the new `cqs index --model` drift detection bails without a structured warn.
2. **Slow-path latency is unattributable** (OB-V1.38-4, OB-V1.38-7). `wait_for_batch` polls Anthropic for 10-30 minutes per LLM pass with zero spans and no elapsed_ms; SPLADE save rollback failures emit error events but no operator-facing surface.
3. **User-actionable info trapped behind RUST_LOG=debug** (OB-V1.38-3, OB-V1.38-5, OB-V1.38-6). TOML overlay loads, GPU/CPU fallback decisions, and skip-first-pass mode are all at debug — operators can't tell from journald whether their config landed, whether queries ran on GPU, or why mid-index search recall is poor.
4. **Warns missing structured fields** (OB-V1.38-8, OB-V1.38-10). Two warns drop the most diagnostic field of their context (reranker kind, chunk_count/dim) — easy adds, but lost in journal triage today.
5. **Dispatch-entry overhead is invisible** (OB-V1.38-9). Three TOML overlay loads run unconditionally; no span aggregates them, so structured-trace consumers can't measure config-load cost across runs.

The pattern across PRs #1505/#1506 is consistent with prior audit waves: feature additions ship with `tracing::warn!(error = %e, ...)` on error paths but skip the entry span. The CLAUDE.md memory rule ("Every new public function must have a `tracing::info_span!` at entry") needs to extend to per-subcommand handlers (`cmd_ref_update`, the dispatch-shim leaves), not just the dispatcher.

---

## Test Coverage (adversarial)

# Audit Batch 1 — Test Coverage (Adversarial)

Audit cut against v1.38.0 + post-v1.38 work (HEAD `fe7126c4`). Prior triage in
`docs/audit-triage.md` (v1.36.2). Skipped: TC P1-21/22/23, P2-4/5/6, P3 TC
Adversarial 5-pack, P4-1, SEC-V1.36-6 query-token-leak (PR #1456),
SPLADE-α bounds (PR #1479-era).

The PRs the prompt cited (#1505 "[index.policy] parser", #1507 "project search
filter", #1511) don't exist in git history. #1505 is actually `cqs index
--model parity + drift detection`; #1506 is `cqs ref reindex` flag parity. No
[index.policy] parser shipped. The findings below cover the real recent code.

---

#### TC-ADV-V1.38-1: `validate_repo_id` `..` rejection has no regression test
- **Difficulty:** easy
- **Location:** `src/cli/commands/train/export_model.rs:33` (rejection); `src/cli/commands/train/export_model.rs:227-263` (test mod)
- **Description:** SEC-V1.36-7 added an explicit `if repo.contains("..")` rejection at line 33 — the comment notes `org/../../etc/secrets` would otherwise let `optimum` walk above CWD. Tests cover TOML injection, leading dash, missing slash, but **no test exercises the `..` path-traversal case**. A future refactor that simplifies the validator (e.g. relies on the char-set whitelist alone) silently regresses the security fix because `.` and `/` are individually allowed.
- **Suggested fix:** add `validate_repo_id_rejects_parent_directory_refs` asserting `validate_repo_id("org/../../etc/passwd").is_err()`, `validate_repo_id("..").is_err()`, `validate_repo_id("foo/..").is_err()`, `validate_repo_id("a..b/c").is_err()` (substring match, even mid-segment).

#### TC-ADV-V1.38-2: `NoteBoostIndex::boost` / `OwnedNoteBoostIndex::boost` NaN/+Inf defenses untested
- **Difficulty:** easy
- **Location:** `src/search/scoring/note_boost.rs:147-153` and `src/search/scoring/note_boost.rs:247-253`
- **Description:** Both `boost()` methods comment-cite `test_upsert_notes_infinity_sentiment_roundtrips` (in `store/notes.rs:497`) but no test calls `NoteBoostIndex::new(&[note_with_inf_sentiment]).boost(...)` to assert the result is `1.0` (or any finite value). P1-36 was about the BoundedScoreHeap drop; the *consumer-side* clamp at the boost call has zero direct coverage. A future inlining or refactor of the clamp can silently revert P1-36 and tests will pass. Production `OwnedNoteBoostIndex` (`Arc<>` cache form) is the actual hot path and has even less coverage than the borrowed variant.
- **Suggested fix:** add `boost_clamps_inf_sentiment_to_finite_multiplier` and `boost_treats_nan_sentiment_as_zero` for both `NoteBoostIndex` and `OwnedNoteBoostIndex` — assert returned multiplier is finite and within `[1.0 - factor, 1.0 + factor]` when sentiment is `f32::INFINITY`, `f32::NEG_INFINITY`, `f32::NAN`.

#### TC-ADV-V1.38-3: `upsert_sparse_vectors` write-path NaN/Inf untested
- **Difficulty:** easy
- **Location:** `src/store/sparse.rs:138-303`
- **Description:** P1-21/31/37 fixed the *read* path (`token_dump_paged` / `load_all_sparse_vectors` filter non-finite). The *write* path at line 218/231 binds `weight: f32` directly via `push_bind(w)` with no `is_finite` check. SQLite REAL accepts `+Inf`/`-Inf`/`NaN` (NaN gets coerced to NULL on the bind, which then violates `NOT NULL`, but Inf goes through). A buggy SPLADE encoder that produces `Inf` would persist to disk and only be filtered by the loader — meaning HNSW rebuild logs warnings forever and the row never disappears. No test confirms that `upsert_sparse_vectors([(c, [(0, f32::INFINITY)])])` either errors or scrubs the value at write time.
- **Suggested fix:** add `test_upsert_sparse_vectors_inf_weight_behavior` and `test_upsert_sparse_vectors_nan_weight_behavior` — assert which contract is in force (either `Err(StoreError::Runtime)` or scrubbed to a valid float). Pin behaviour so a future write-side scrubber is a deliberate change.

#### TC-ADV-V1.38-4: `lookup_main_cqs_dir` "stray .cqs FILE" path lacks test
- **Difficulty:** easy
- **Location:** `src/worktree.rs:135` (P1-43 fix); `src/worktree.rs:333-410` (test mod)
- **Description:** P1-43 changed `own_cqs.exists()` → `own_cqs.is_dir()` so a stray *file* named `.cqs` (mistaken `touch .cqs`, packaged tarball with the wrong entry) doesn't get treated as an index dir. None of the four worktree tests covers this — `lookup_prefers_own_cqs_dir` only creates `.cqs/` as a *directory*. A regression to `.exists()` passes all current tests but reintroduces the "is not a directory" downstream confusion.
- **Suggested fix:** add `lookup_ignores_stray_cqs_file_falls_through` — `std::fs::write(dir.path().join(".cqs"), b"")`, then assert `lookup_main_cqs_dir(dir.path())` returns `MainIndexLookup::NotWorktree` (not `OwnIndex`).

#### TC-ADV-V1.38-5: `write_active_slot` concurrent-writer race fix has no regression test
- **Difficulty:** medium
- **Location:** `src/slot/mod.rs:647-694` (DS-V1.33-2 fix at lines 664-669)
- **Description:** DS-V1.33-2 added `crate::temp_suffix()` so two concurrent `write_active_slot` callers (legacy migration + `cqs slot promote`) each stage to their own temp file before `atomic_replace`. Before the fix, both raced on `active_slot.tmp` and one would clobber the other's temp before the rename. The slot test mod has zero `thread::spawn` calls — DS-V1.33-2 is implicitly tested by the round-trip test, which can't observe the race. A revert that removes the suffix passes all tests.
- **Suggested fix:** add `write_active_slot_two_concurrent_writers_dont_corrupt` — spawn 8 threads × 50 writes each with `slot_a` and `slot_b` interleaved, `join` all, then assert the final file content is *exactly one of* `"slot_a"` or `"slot_b"` (not partial / empty / mixed bytes). This catches both the temp-collision regression and an atomic-replace contract violation.

#### TC-ADV-V1.38-6: `check_index_model_drift` case-insensitive / whitespace edges untested
- **Difficulty:** easy
- **Location:** `src/cli/commands/index/build.rs:1501-1524` (PR #1505)
- **Description:** Drift detection uses byte-exact `==` for `stored_name == requested_name || stored_name == requested_repo`. No test for: (a) `--model BGE-Large` vs stored `bge-large` — would erroneously bail, sending the operator into a `--force` rebuild they didn't actually need; (b) trailing newline / whitespace in stored value (could happen if a future migration writes `format!("{}\n", name)`); (c) empty `requested_name` / `requested_repo` (passes through with `Some(stored) != ""`); (d) stored with embedded NUL byte. Operator-facing footgun: silent over-trigger of drift could destroy a 30-min HNSW build for a cosmetic name diff.
- **Suggested fix:** add `check_index_model_drift_is_case_sensitive_pin` (current contract is case-sensitive — pin it so a future case-fold is deliberate); `check_index_model_drift_rejects_whitespace_drift` (stored = `"bge-large\n"` vs requested = `"bge-large"` — pin behaviour); `check_index_model_drift_empty_requested_bails`.

#### TC-ADV-V1.38-7: `embed_query` oversized-input truncation path untested
- **Difficulty:** easy
- **Location:** `src/embedder/mod.rs:965-981` (RT-RES-5)
- **Description:** RT-RES-5 truncates queries longer than `max_query_bytes()` at a UTF-8 char boundary. No test exercises: (a) `text.len() > max_query_bytes` — truncate path never runs in tests; (b) multi-byte boundary handling — a query of `max_query_bytes - 1` bytes followed by a 4-byte emoji that crosses the cap. The `is_char_boundary` walk has no asymptotic bound (`while ... end > 0`) but in practice converges in ≤4 iterations — still a behaviour worth pinning. A future regression that uses byte-slicing without the boundary check would panic on UTF-8 violation in production.
- **Suggested fix:** add `test_embed_query_truncates_oversized_input_at_char_boundary` — set `CQS_MAX_QUERY_BYTES=100`, then `embed_query("a".repeat(99) + "🦀")` (4-byte emoji at boundary 99); assert it returns `Ok(_)` and doesn't panic. Add `test_embed_query_truncate_fits_under_cap` asserting the byte length actually used is `<= cap`.

#### TC-ADV-V1.38-8: `score_candidate` `threshold = NaN` / `±Inf` behaviour untested
- **Difficulty:** easy
- **Location:** `src/search/scoring/candidate.rs:371` (`if score >= ctx.threshold`)
- **Description:** `threshold: f32` flows from `search_filtered(.., threshold)` straight into `score >= ctx.threshold`. With `threshold = f32::NAN`, IEEE-754 says `score >= NaN` is always false → silent empty result set. With `threshold = f32::NEG_INFINITY`, all candidates pass; with `+Inf`, none pass. None of these are tested — and `search_filtered` doesn't validate or clamp threshold at the public boundary either. A caller that computes threshold via `(score / count) as f32` where `count = 0` produces NaN and the daemon returns `[]` with no error.
- **Suggested fix:** add `test_search_filtered_nan_threshold_returns_empty_not_error` (current contract — pin so future `is_finite` rejection is a deliberate change), `test_search_filtered_neg_inf_threshold_returns_all`, `test_search_filtered_inf_threshold_returns_empty`. Even better: add validation at `search_filtered` entry that bails on non-finite threshold; tests then assert `Err`.

#### TC-ADV-V1.38-9: `load_synonym_overlay` / `load_classifier_vocab_overlay` `MAX_BYTES` cap untested
- **Difficulty:** easy
- **Location:** `src/search/synonyms.rs:120,135` and `src/search/router.rs:586,601`
- **Description:** Both overlays cap at 4 KiB via `take(MAX_BYTES).read_to_string(...)`. If a 100 MiB hostile config landed on disk, the cap saves the indexer from OOM. No test writes a > 4 KiB file to verify the cap actually kicks in — current "malformed TOML" tests use a few-byte payload. A future refactor that drops the `.take(MAX_BYTES)` (e.g. switching to `fs::read_to_string`) silently reintroduces the unbounded read.
- **Suggested fix:** for each overlay add `loader_truncates_at_max_bytes` — write a 5 KiB file with a valid `[synonyms]` / `[classifier]` table at the start, then ~4 KiB of `# comment` padding, then a *malformed* TOML marker after byte 4096. Assert the loader returns the table from the first 4 KiB without falling into the malformed-TOML branch (or assert the alternative — that truncation mid-table produces empty due to malformed parse). Either way, pin which contract is in force.

#### TC-ADV-V1.38-10: `embedding_to_bytes` write-path doesn't validate finiteness
- **Difficulty:** easy
- **Location:** `src/store/helpers/embeddings.rs:15-27`
- **Description:** The write path checks dim but not finiteness — `bytemuck::cast_slice` happily serializes NaN/Inf bytes. `Embedding::try_new` would have caught these, but `Embedding::new` (the unchecked constructor) is used at 19 sites in `src/` (see `cqs callers Embedding::new`). Loading-side test `test_embedding_slice_passes_nan_bytes_through` deliberately pins the read-side passthrough; the write-side equivalent ("can NaN actually reach disk?") has no test. If any of those 19 unchecked constructors gets a NaN value (from a buggy reranker, drift normalization, etc.), the bytes land in SQLite undetected and corrupt every cosine score that touches the chunk.
- **Suggested fix:** add `test_embedding_to_bytes_nan_input_behavior` and `test_embedding_to_bytes_inf_input_behavior` — call `embedding_to_bytes(&Embedding::new(vec![f32::NAN; DIM]), DIM)`; assert which contract holds (current: `Ok` with NaN bytes; better: `Err(StoreError::Runtime)`). Pin behaviour, then file a follow-up to flip to the rejecting form mirroring `Embedding::try_new`.

---

## Summary

10 findings. All easy except TC-ADV-V1.38-5 (medium, requires multi-thread harness).

Themes:
- **Comment-cited fixes without regression tests** (TC-ADV-V1.38-1 SEC-V1.36-7 `..`,
  TC-ADV-V1.38-2 P1-36 boost clamp, TC-ADV-V1.38-4 P1-43 `is_dir`,
  TC-ADV-V1.38-5 DS-V1.33-2 race) — 4 of 10. The fix shipped, the test
  didn't. A revert-style regression passes all current tests.
- **NaN/Inf passthrough cluster sequel** (TC-ADV-V1.38-2, -3, -8, -10) — v1.36.2
  closed many of these on read paths; write paths and consumer-side clamps
  remain undertested. `embedding_to_bytes` is the highest-impact: a NaN that
  reaches SQLite poisons every search touching the chunk and only surfaces
  via cosine-NaN drops downstream.
- **Recent-PR adversarial gaps** (TC-ADV-V1.38-6 #1505 drift, TC-ADV-V1.38-9
  PR #1482/#1483 overlay caps) — happy-path tests exist; edge cases don't.

Prioritization: TC-ADV-V1.38-1 first (security fix without test), then
TC-ADV-V1.38-10 (write-side data-corruption surface), then -2/-3/-5
(claimed-fixed contracts).

---

## Robustness

## Robustness

Audit pass against the post-v1.38.0 main branch (4a31285e). RB-V1.36-*
items from prior triage are excluded. Production unwrap surface is small
(~25 sites), but a handful of latent panics live in user/external-input
paths (worktree, daemon reindex, SPLADE on-disk index, malicious config
files) and a few `unwrap_or` patterns are sibling-level misses of
already-fixed bounds.

#### RB-V1.38-1: `worktree::resolve_main_project_dir` reads `commondir` unbounded — sibling of `RB-V1.33-2`
- **Difficulty:** easy
- **Location:** `src/worktree.rs:91`
- **Description:** `MAX_GIT_FILE_BYTES = 4 KiB` is correctly applied to the
  `.git` link file at line 67–71 (`File::open ... .take(MAX_GIT_FILE_BYTES)`),
  but the very next read — `<gitdir>/commondir` at line 91 — uses
  `std::fs::read_to_string(&commondir_file)` with no cap. `commondir` is a
  git-internal file that normally contains `../..` (~6 bytes), but the path
  is computed from the worktree's untrusted `.git` file (`gitdir:` line) and
  fed to `read_to_string`. A worktree pointing at a hostile gitdir whose
  `commondir` is a multi-GB file (or a FIFO) OOMs / hangs every cqs command
  invoked from inside the worktree. Resolves on every CLI call that hits
  `resolve_index_dir`, so this fires on cold-paths the daemon can't shield.
- **Suggested fix:** Same shape as the `.git` reader above —
  `File::open(&commondir_file).ok()?.take(MAX_GIT_FILE_BYTES).read_to_string(&mut buf).ok()?`.
  Real `commondir` content is < 100 bytes; 4 KiB cap is ample.

#### RB-V1.38-2: `cli/watch/reindex.rs:626` panics the daemon on chunk-index mismatch
- **Difficulty:** medium
- **Location:** `src/cli/watch/reindex.rs:617-627`
- **Description:** Watch-mode reindex merges `cached` and `to_embed` into a
  `HashMap<usize, Embedding>` keyed by chunk index, then rebuilds the
  per-chunk vector with `(0..chunk_count).map(|i| by_index.remove(&i).unwrap_or_else(|| panic!(...)))`.
  The comment claims it's unreachable, but a partial embedder failure where
  `new_embeddings.len() != to_embed.len()` (e.g. ORT session error mid-batch,
  or any future code path that returns a short Vec) lands directly in the
  panic arm and kills the daemon. The watch loop is the daemon's hot path —
  a single bad embedder run takes down `cqs-watch` until systemd restarts.
- **Suggested fix:** Return `Err(WatchError::ReindexInvariant { chunk_index, chunk_count })`
  via `?` so the watch loop logs the violation and skips this file rather
  than crashing. The caller already handles per-file errors; the panic is
  needlessly fatal for a recoverable invariant break.

#### RB-V1.38-3: `splade/index.rs` load-path uses `.try_into().unwrap()` 9× on body slices
- **Difficulty:** easy
- **Location:** `src/splade/index.rs:703,710,717,718,719,781,819,822,828,830`
- **Description:** Each of these is `u32::from_le_bytes(slice[a..b].try_into().unwrap())`
  (or `u64`/`f32`/`[u8; 32]` variants) on slices read from a SPLADE on-disk
  index. By construction every site is preceded either by the
  fixed-`SPLADE_INDEX_HEADER_LEN` header read or by a `need(&body, cursor, n)?`
  bound check, so the unwraps are provably safe today. They violate the
  project's "no `unwrap()` outside tests" rule and silently lose the audit
  trail — a future refactor that drops a `need()` call leaves no compile-time
  signal that the corresponding `try_into().unwrap()` becomes a panic on a
  malformed index. SPLADE indexes are loaded from disk (`.cqs/splade.idx`),
  so the input is reachable by anyone who can write to `.cqs/`.
- **Suggested fix:** Replace each with
  `u32::from_le_bytes(slice[a..b].try_into().expect("invariant: header[4..8] is exactly 4 bytes"))`
  to surface the invariant in the binary's panic messages, OR thread the
  `try_into()` result into the existing `SpladeIndexPersistError::CorruptData`
  return path with a single helper (`fn read_le_u32(body: &[u8], cursor: usize) -> Result<u32, ...>`).
  The helper version costs one closure call per field and removes the panic
  surface entirely.

#### RB-V1.38-4: `cli/commands/infra/doctor.rs:582` `api_base.unwrap()` is brittle by-flow-only
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/doctor.rs:582`
- **Description:** `let base = api_base.unwrap();` is reachable today only
  because the `match api_base.as_deref() { _ => return; ... }` block at
  lines 547-563 returns on `None`. The unwrap holds because of an earlier
  early-return, not because of a type-level guarantee — a future refactor
  that turns the early return into a `*any_failed = true; ` (without
  `return`) silently converts this into a panic on `cqs doctor` (a user-facing
  diagnostic command — it must not crash). This is exactly the pattern
  CLAUDE.md flags as an anti-pattern.
- **Suggested fix:** Hoist the value out of the first match instead of
  re-reading: `let base = match api_base.as_deref() { Some(s) if !s.is_empty() => s.to_string(), _ => { /* current err branch */ return; } };`.
  This makes the value's existence a type-level fact at line 582.

#### RB-V1.38-5: `cli/commands/index/umap.rs:122` capacity computation overflows on pathological input
- **Difficulty:** medium
- **Location:** `src/cli/commands/index/umap.rs:122`
- **Description:** `Vec::with_capacity(12 + n_rows * (2 + id_max_len + dim * 4))`.
  The `ensure!` block above only validates each operand fits in `u32` (≤
  4.3e9). On a 64-bit host the product can reach ~9.2e19, well past
  `usize::MAX` (1.8e19). In release builds usize multiplication wraps
  silently and `Vec::with_capacity` allocates a misleadingly small buffer
  before `extend_from_slice` panics on out-of-bounds memory. In debug builds
  the multiplication panics directly. The current corpus is far from this
  bound, but the validation is in u32-space rather than product-space, so a
  malicious / huge index can drive the multiplication into UB territory.
- **Suggested fix:** Replace with checked arithmetic and `anyhow::ensure!`
  before `with_capacity`:
  `let cap = 12usize.checked_add(n_rows.checked_mul(2usize.checked_add(id_max_len)?.checked_add(dim.checked_mul(4)?)?)?).ok_or_else(|| anyhow::anyhow!("UMAP payload size overflows usize"))?;`
  Or simply pre-compute on `u128` and reject if `cap > 1 GiB` (the actual
  IPC budget).

#### RB-V1.38-6: `parser/l5x.rs` regex-capture `.unwrap()` cluster (6 sites)
- **Difficulty:** easy
- **Location:** `src/parser/l5x.rs:263,264,366,367,368,387`
- **Description:** Six `.get(N).unwrap()` calls on `regex::Captures`:
  ```rust
  let full = st_match.get(0).unwrap();
  let inner = st_match.get(1).unwrap();
  ...
  let routine_name = block.get(1).unwrap().as_str().to_string();
  let block_content = block.get(2).unwrap().as_str();
  let block_start = block.get(0).unwrap().start();
  ...
  let inner = st_block.get(1).unwrap().as_str();
  ```
  Every regex involved (`L5X_ST_CONTENT_RE`, `L5K_ROUTINE_BLOCK_RE`,
  `L5K_ST_CONTENT_BLOCK_RE`) defines unconditional capture groups, so groups
  1 and 2 are always present when the parent match exists — the unwraps are
  semantically safe. They still violate the project rule and would not
  survive a regex tweak that adds an alternation or makes a group optional.
  L5X parsing is invoked on every Rockwell `.l5x` / `.l5k` source the user
  hands cqs (via `cqs index`); a hostile/corrupt file with a regex-quirk
  edge case (zero-width match, anchor weirdness) could expose an
  inconsistency.
- **Suggested fix:** Replace with `match block.get(1) { Some(m) => m.as_str().to_string(), None => continue, }` (skip the malformed routine, parser already accepts partial input). For `block.get(0)` use `let block_start = st_match.get(0).map(|m| m.start()).unwrap_or(0);`.

#### RB-V1.38-7: `train_data/query.rs:14` static regex `.unwrap()` inconsistent with siblings
- **Difficulty:** easy
- **Location:** `src/train_data/query.rs:14`
- **Description:** Three `LazyLock<Regex>` siblings in the same file
  (`conventional_prefix_re`, `trailing_noise_re`, lines 7 and 21) use
  `.expect("valid regex")`; the leading-verb regex at line 14 uses bare
  `.unwrap()`. The regex is a 90+ alternation built by hand — if a typo
  ever lands (an unbalanced `(`, a malformed character class), the panic
  message gives no breadcrumb. Trivial inconsistency, easy to fix, matches
  existing style.
- **Suggested fix:** `Regex::new(...).expect("valid leading-verb regex")`.

#### RB-V1.38-8: `embedder/models.rs:684,691,741` `.expect("guarded by has_X")` on `Option`
- **Difficulty:** easy
- **Location:** `src/embedder/models.rs:684,691,741`
- **Description:** Three sites: `embedding_cfg.dim.expect("guarded by has_dim")`,
  `embedding_cfg.repo.as_ref().expect("guarded by has_repo")`,
  `embedding_cfg.repo.clone().expect("guarded by has_repo")`. The `has_*`
  flags are local `bool` variables computed at lines 681-682 with `.is_some()`
  and gated by `if has_repo && has_dim`, so the expect calls are safe today.
  But the guard and the unwrap are in different scopes and there's no
  type-level invariant — a future contributor refactoring the validation
  block (e.g., adding a dim==0 check that mutates `dim` to None on the way
  through) could silently turn the expect into a panic on user-supplied
  `cqs.toml`. Custom-model TOML config is **user input**, so this is a
  practical robustness concern.
- **Suggested fix:** Bind the unwrapped values once at the top of the
  validation block: `let (Some(repo), Some(dim)) = (&embedding_cfg.repo, embedding_cfg.dim) else { return Self::default_model(); };` (or equivalent if-let-chain), then use `repo` and `dim` directly throughout. Removes the guard/unwrap split.

#### RB-V1.38-9: `nl/fields.rs:122` `unreachable!()` reachable on field-style additions
- **Difficulty:** easy
- **Location:** `src/nl/fields.rs:122`
- **Description:** `FieldStyle::None => unreachable!()` is genuinely
  unreachable today — line 84 returns early on `FieldStyle::None`. But the
  match at line 95 is non-exhaustive over future variants: if a fourth
  field style is added to `FieldStyle` without a return-early shortcut at
  line 84, the match falls through to `unreachable!()` and panics on every
  source file in that language. `FieldStyle` is a project-internal enum
  that new languages routinely touch; the early-return discipline is
  enforced at runtime, not by the type system.
- **Suggested fix:** Either swap to `_ => return Vec::new()` (matches the
  line-84 fall-through behavior), or move the `FieldStyle::None` arm into
  the match itself (`FieldStyle::None => return None` inside the match). The
  latter restores exhaustiveness so a new variant is a compile error, not
  a runtime panic.

#### RB-V1.38-10: `cli/watch/mod.rs:1654,1695` `handle_opt.take().unwrap()` brittle to refactor
- **Difficulty:** easy
- **Location:** `src/cli/watch/mod.rs:1654,1695`
- **Description:** Both call sites are inside a
  `match handle_opt.as_ref() { Some(h) if h.is_finished() => { handle_opt.take().unwrap().join() ... } }`
  pattern. The unwrap holds because the `Some(h)` arm guarantees the
  Option is `Some` at the point `take()` is called — but only because
  Rust treats `.as_ref()` views and `.take()` operations on the same
  binding as referring to the same value. A refactor that turns
  `handle_opt` into a re-bound value or that adds an early intervening
  `take()` (e.g. for a deadline-cancellation check) silently turns this
  into a daemon-shutdown panic. The daemon is mid-shutdown when this
  fires, so a panic during shutdown can leave socket files / lock files
  orphaned (see existing `daemon_socket` cleanup ordering). The watch
  module is the project's most operationally sensitive component.
- **Suggested fix:** Use `if let Some(handle) = handle_opt.take()` directly
  instead of the match-arm-then-take-unwrap dance:
  ```rust
  if let Some(handle) = handle_opt.take() {
      if handle.is_finished() {
          if let Err(e) = handle.join() { ... }
          break;
      } else {
          handle_opt = Some(handle);  // put it back
      }
  }
  ```
  Or split into a `try_join_with_deadline` helper that owns the join
  semantics and never exposes the unwrap to the call site.


---

## Scaling & Hardcoded Limits

## Scaling & Hardcoded Limits — v1.38.0+ post-PR-#1503

Prior SHL-V1.36-1..10 are all fixed (verified `store/mod.rs:734-745`,
`reference.rs:208`, `cli/commands/index/build.rs:1306`, `cagra.rs`,
`reranker.rs:89-112`, `cli/watch/reindex.rs:236`, `lib.rs:475` (32766
limit text), `cli/pipeline/types.rs:143-158`, `embedder/mod.rs:343`,
`cli/watch/socket.rs:51-61`).

The findings below are NEW since v1.36.2 — none of them duplicate the
SHL-V1.36 series.

#### SHL-V1.38-1: `MAX_PENDING_REBUILD_DELTA = 5_000` doesn't scale with embedding dim
- **Difficulty:** easy
- **Location:** `src/cli/watch/rebuild.rs:81-87`
- **Description:** Cap on per-rebuild HNSW delta entries. Comment explicitly says "5,000 × 1024 dim × 4 bytes ≈ 20 MB worst case" — same dim-blind anti-pattern as SHL-V1.36-3/4/5 (which were fixed). At 4096-dim (Qwen3-style) this becomes 80 MB held in memory until the next swap; at SPLADE-Code 1024-hidden / 2560-output it's larger still. No env override exists, so an operator on a wide-dim model can't shrink the cap. The comment's own arithmetic outdates the constant.
- **Suggested fix:** Pull the same `cqs::limits::dim_scaled_batch(5_000, dim, 500, 50_000)` helper used by `hnsw_batch_size` (`build.rs:1306-1311`), reading `dim` from the rebuild context's `store.dim()`. Add `CQS_PENDING_REBUILD_DELTA_MAX` env override matching the other `CQS_HNSW_*` knobs.

#### SHL-V1.38-2: `--require-fresh-secs` silently capped at hardcoded `600u64` literal
- **Difficulty:** easy
- **Location:** `src/cli/commands/eval/mod.rs:294-307`
- **Description:** SHL-V1.30-3 was fixed in #1235 by adding a warn — but the cap value itself is still a `600u64` literal hardcoded twice in the source (line 294 and 297, with the message "capped at 600 s (built-in eval ceiling)"). The `wait_for_fresh` defense-in-depth ceiling is `86_400 s`, so the eval-side ceiling has 144× headroom but won't budge. On a fresh checkout that triggers a full reindex of a 100K-chunk repo, embedder warmup + index build can exceed 10 min — the eval gate then fails after 600s of waiting even when the operator passed `--require-fresh-secs 1800`. The clamp is documented but not overridable.
- **Suggested fix:** Replace `600u64` with `crate::limits::parse_env_u64("CQS_EVAL_FRESH_BUDGET_CEILING", 600)`, mirroring the `CQS_*` env-knob pattern used everywhere else. Keep the warn (it's correct UX), but let an operator on a slow indexer push the ceiling up.

#### SHL-V1.38-3: `PIPELINE_FAN_OUT_LIMIT = 50` silently truncates batch pipelines, no env override
- **Difficulty:** easy
- **Location:** `src/cli/batch/pipeline.rs:10-12, 269-278, 340-347`
- **Description:** Pipeline command (`cqs callers foo | scout`) caps fan-out at 50 names per stage. Truncation is logged at `tracing::info`, but the limit itself is hardcoded. With Claude Code Tasks dispatching agents that build pipelines from `cqs callers <hub>` (>100 callers on hot functions like `Store::search_filtered` or `Embedder::embed_query`) the silent truncation drops half the call graph downstream. No `CQS_PIPELINE_FAN_OUT` knob; comment says "3-stage pipeline dispatches at most 1 + 50 + 50 = 101 calls" — but 101 is not the cost driver, the inner per-call latency (~50ms via daemon) is.
- **Suggested fix:** Add `CQS_PIPELINE_FAN_OUT` env knob with default 50, clamping `[10, 1000]`. Consider raising default to 100 — agents are the primary user and a 100-name fan-out is ~5s at daemon latency, not painful.

#### SHL-V1.38-4: Daemon socket request line capped at 1 MB while CLI accepts 50 MB
- **Difficulty:** medium
- **Location:** `src/cli/watch/socket.rs:113, 124` vs `src/cli/limits.rs:90`
- **Description:** `cqs review --stdin` and `cqs impact --diff` accept `MAX_DIFF_BYTES = 50 * 1024 * 1024` (env-overridable via `CQS_MAX_DIFF_BYTES`) on the CLI path. The same commands routed through the daemon hit a hardcoded 1 MB cap on the socket line (`take(1_048_577)` + post-hoc `n > 1_048_576`). Operators with a 5 MB squash-merge diff get `TooLarge` when the daemon is up, success when it's down — exactly the pre-CQ-V1.25-2 anti-pattern that drove the existence of `cli/limits.rs` in the first place. No env override on the daemon side either.
- **Suggested fix:** Replace the literal pair with `cli::limits::max_diff_bytes()` (the same resolver used by the CLI path) plus a small JSON-envelope overhead (~1 KB). The 1 MB ceiling came from "scout / status take a few KB"; review/impact are now first-class clients of the same socket and need the larger budget.

#### SHL-V1.38-5: `STREAM_BATCH_SIZE = 1024` in UMAP path is dim-blind with no env override
- **Difficulty:** easy
- **Location:** `src/cli/commands/index/umap.rs:21, 86`
- **Description:** UMAP projection paginates `embedding_batches(1024)`. At 1024-dim (BGE-large) each batch is ~4 MB; at 4096-dim it's ~16 MB; at hypothetical 8192-dim it's 32 MB. `cqs index --umap` is opt-in (#1452 v22 schema), but on a wide-dim eval slot this drives heap higher than necessary. No comment explains why 1024 was picked, and the `payload` `Vec::with_capacity(12 + n_rows * (2 + id_max_len + dim * 4))` two lines down already shows dim-awareness — the inconsistency is the smell.
- **Suggested fix:** Make this `cqs::limits::dim_scaled_batch(1_024, dim, 64, 8_192)` and add a `CQS_UMAP_STREAM_BATCH` env knob. Cheap fix and matches the pattern established by SHL-V1.36-3/4/5.

#### SHL-V1.38-6: `parse_channel_depth() = 512` default ignores file size and chunk fan-out
- **Difficulty:** easy
- **Location:** `src/cli/pipeline/types.rs:115-130`
- **Description:** Parse stage channel buffers up to 512 `ParsedBatch` messages. Each batch holds a `Vec<Chunk>` for one file_batch slice (up to `file_batch_size() = 5_000` files), each chunk carries a `String content` (~1-10 KB typical). Worst-case buffered: 512 × 100 chunks/file × 5 KB = 256 MB. The 512 default has no scaling rationale — it's a guess from when `file_batch_size` was 1000. SHL-V1.36-8 fixed `embed_channel_depth` by deriving from a *byte budget*; the parse channel deserves the same treatment for the same reason.
- **Suggested fix:** Mirror `embed_channel_depth`: pin a byte budget (e.g., 32 MB), derive depth from `(file_batch_size() × estimated chunks/file × estimated bytes/chunk)`. `CQS_PARSE_CHANNEL_DEPTH` env override stays. Less aggressive: just halve to 256 (still env-overridable) — the queue rarely backs up since parsing is faster than embedding.

#### SHL-V1.38-7: LLM-pass `PAGE_SIZE = 500` literal duplicated in two files, no env override
- **Difficulty:** easy
- **Location:** `src/llm/mod.rs:94`, `src/llm/doc_comments.rs:164`
- **Description:** Two production LLM-pass paginators hard-code `PAGE_SIZE = 500` for `chunks_paged(cursor, PAGE_SIZE)`. SHL-V1.30-8 added `CQS_ENRICHMENT_PAGE_SIZE` for the parallel enrichment paginator (`src/cli/enrichment.rs`); these LLM-pass paginators were missed. On large repos (>100k chunks) the page count is `total / 500 = 200+ round-trips`, each fetching `ChunkSummary` (with content). With `--llm-summaries` running for hours, a smaller page (50-100) reduces peak heap; with a fast SSD a larger page reduces SQLite round-trip overhead. Operators can't tune either way.
- **Suggested fix:** Extract a single `crate::limits::llm_pass_page_size()` resolver reading `CQS_LLM_PASS_PAGE_SIZE` (default 500), used by both call sites. Unifies with `enrichment_page_size()` patterning.

#### SHL-V1.38-8: Reconcile streaming path `BATCH = 1000` files hardcoded
- **Difficulty:** easy
- **Location:** `src/cli/watch/reconcile.rs:342, 350-355`
- **Description:** `#1229 (RM-5)` streaming reconcile path buffers 1000 paths at a time. Comment claims "Peak heap is `O(BATCH)` — independent of tree size", but BATCH itself is hardcoded with no env. On a small repo (<5000 files) BATCH=1000 means ~5 reconcile steps, each issuing an N-row `IN (...)` SELECT against `chunks` — already pretty good. On a 200k-file monorepo it's 200 round-trips. SQLite handles `IN (?...)` with `max_rows_per_statement(N)` ceilings, so 1000 is fine for the SQL side, but operators on either extreme can't tune.
- **Suggested fix:** Add `CQS_RECONCILE_BATCH` env override (default 1000), clamping `[100, 32_000]` (latter aligns with the sql.rs SQLite ceiling).

#### SHL-V1.38-9: `summary_queue` thresholds (64/200ms/10_000) hardcoded with no env override
- **Difficulty:** medium
- **Location:** `src/store/summary_queue.rs:99, 103, 108`
- **Description:** `DEFAULT_FLUSH_THRESHOLD_ROWS = 64`, `DEFAULT_FLUSH_INTERVAL_MS = 200`, `HARD_CAP_ROWS = 10_000` are all fixed. Comment says "Starting guess: `N=64, T=200ms`. Run a benchmark on the local LLM path before committing to numbers" — i.e., explicitly punted on tuning. With Anthropic Batches finishing in ~5 min (no streaming) the queue stays below 64 rows trivially. With the local vLLM provider (`feedback_vllm_gemma.md`, ~50 chunks/sec sustained) the queue can hit 64 in <2 sec, triggering a flush every 2 sec. That's fine, but tunable would let an operator on a slow disk push to 256+/500ms.
- **Suggested fix:** Three env knobs: `CQS_SUMMARY_FLUSH_ROWS`, `CQS_SUMMARY_FLUSH_INTERVAL_MS`, `CQS_SUMMARY_HARD_CAP_ROWS`. Defaults unchanged. Wire through a single `summary_queue_config()` helper to keep all three reads in one place.

#### SHL-V1.38-10: `RETRY_BACKOFFS_MS` schedule is a hardcoded `&[u64]` slice
- **Difficulty:** easy
- **Location:** `src/llm/local.rs:48-50`
- **Description:** Local LLM provider retry schedule is `&[500, 1000, 2000, 4000]` — 7.5s window total. Hardcoded, not even a const + env override. With local vLLM serving on a saturated GPU, transient 503 / connection-reset bursts can exceed 7.5s; the request fails after 4 tries instead of riding through the burst. Cousin sites (`embedder/mod.rs` ORT init backoff, `cli/watch/rebuild.rs::EmbedderBackoff` exponential to 5min) take very different shapes — the LLM path is the most fragile and got the most aggressive ceiling.
- **Suggested fix:** Add `CQS_LLM_RETRY_BACKOFFS_MS` parsing a comma-separated list (e.g. `"500,1000,2000,4000,8000"`); fall through to the current default. Optional: separate `CQS_LLM_RETRY_MAX_ATTEMPTS` knob so the slice length and `MAX_ATTEMPTS` (line 50) stay in sync without source edits.

---

## Algorithm Correctness

# Algorithm Correctness — v1.38.x audit

Scope: post-#1456 / v1.38.0 work. Recent merges in focus: #1502 (HNSW Vec<Box<str>>),
#1505 (drift check), #1507 (project search filter merge), #1508 (exhaustive match),
#1509 (try_classify chain), #1510 (prompt envelope), #1511 ([index.policy] resolution).

Closed AC items from prior audits are NOT re-reported. Targeted at **algorithmic** /
boundary / off-by-one / sort-order issues introduced or surviving in the recent diffs.

---

#### AC-V1.38-1: `BoundedScoreHeap::would_accept` violates the tied-score id-tiebreak invariant — silently loses smaller-id boundary candidates in SPLADE search
- **Difficulty:** easy
- **Location:** `src/search/scoring/candidate.rs:234-247` (`would_accept`); caller in `src/splade/index.rs:253-258`
- **Description:** PERF-V1.36-9 added `would_accept` as a pre-flight gate so SPLADE can skip cloning chunk-id strings for candidates that won't enter the heap. The contract `BoundedScoreHeap` itself documents (`candidate.rs:142-149`, the AC-V1.25-1/2 fix) is "deterministic on id ascending — when scores are equal, the smaller id wins." But the new gate uses strict `score.total_cmp(worst_score).is_gt()` for the at-capacity branch, which returns `false` on tied scores. The follow-up `push()` would still evict on `id < worst_id` for that tie — but `would_accept == false` means the SPLADE caller `continue`s and never even calls `push()`. Net effect: **at the eviction boundary, a smaller-id incoming chunk that should evict the largest-id heap entry is silently dropped instead.** The bug only triggers when (a) the heap is full, (b) two SPLADE candidates have *exactly* the same accumulated dot-product score, and (c) one of them has a smaller chunk_id than the worst current entry. Tied dot-products are common at small k against large posting lists, so this is exercised more than it sounds. Pre-PERF-V1.36-9 the SPLADE path went straight to `push`, which respected the invariant.

  The bonus footgun: `would_accept(any)` returns `true` when `capacity == 0` (because `peek()` is None and the `else { true }` arm fires), but `push` silently rejects everything at capacity 0. The mismatch is harmless for SPLADE (caller never builds k=0), but any future caller that trusts `would_accept`'s return value can't.
- **Suggested fix:** In the at-capacity arm, mirror `push`'s comparator exactly: `match score.total_cmp(worst_score) { Greater => true, Equal => id_would_be_less, Less => false }`. Since `would_accept` doesn't know the incoming id, either (a) take `id: &str` as a second parameter and do the full tiebreak, or (b) return `true` for the Equal case and accept that ~1 cheap clone per tied score makes it through to be evaluated by `push`. Option (b) is the smaller diff and preserves the determinism invariant. Add a regression test against the SPLADE caller path with constructed tied scores reaching the boundary in reverse-id order.

---

#### AC-V1.38-2: `mmr_rerank` tie-break uses raw `f32 ==` instead of `total_cmp` — same anti-pattern AC-V1.30.1-7 fixed in `BoundedScoreHeap`
- **Difficulty:** easy
- **Location:** `src/search/mmr.rs:94-97`
- **Description:** The MMR per-candidate selection loop picks the best by `if mmr > best_mmr || (mmr == best_mmr && best_idx == usize::MAX)`. The `==` on raw `f32` is the exact pattern AC-V1.30.1-7 retired in `BoundedScoreHeap::push` (PR #1239). Two issues: (1) on NaN scores both `>` and `==` return `false`, so a NaN MMR silently skips that candidate and the loop may exit with `best_idx == usize::MAX`, returning fewer than `limit` results without any log or warning. (2) The intent ("only initialize on first iter") is encoded as `best_idx == usize::MAX`, which means subsequent ties never replace the first-seen winner — that's deterministic only because `i` iterates ascending. If the candidate slice is later sorted differently upstream (or if the MMR function is reused on a HashMap-derived iterator), the tie-break stops being i-ascending and the result becomes process-seed-randomized.
- **Suggested fix:** Replace the float comparison with `total_cmp`: `match mmr.total_cmp(&best_mmr) { Greater => true, Equal => best_idx == usize::MAX, Less => false }`. Add `debug_assert!(mmr.is_finite())` after the score computation so a NaN candidate score (which would imply an upstream bug) is loud rather than silent, and document that the tie-break depends on the input slice ordering.

---

#### AC-V1.38-3: `bfs_expand` seed sort uses `partial_cmp().unwrap_or(Equal)` instead of `total_cmp`
- **Difficulty:** easy
- **Location:** `src/gather.rs:323-329`
- **Description:** AC-V1.29-3 made `bfs_expand` sort its seed queue by `(score desc, name asc)` so the BFS expansion order was deterministic across HashMap seed iteration. The implementation uses `b_score.partial_cmp(a_score).unwrap_or(Ordering::Equal)`. For finite scores this works, but: (1) a NaN score (result of an upstream `cosine_similarity` against a degenerate vector — possible if an enriched embedding base wasn't recomputed) makes `partial_cmp` return `None`, the seed becomes "equal to everyone", and the secondary `name asc` tiebreak takes over — but the position depends on where in the input slice the NaN sits relative to other equal-tagged entries, which is not stable under sort. (2) `total_cmp` is what the rest of the search/scoring pipeline standardised on (`BoundedScoreHeap`, `apply_parent_boost` re-sort, `search_across_projects` merge); `bfs_expand` is the lone outlier still using `partial_cmp.unwrap_or(Equal)`.
- **Suggested fix:** `b_score.total_cmp(a_score).then_with(|| a_name.cmp(b_name))`. Same change pattern as PR #1239 applied across the rest of the codebase.

---

#### AC-V1.38-4: `try_classify_negation` priority 1 fires on bare common nouns ("no", "exclude") that aren't actually negation context
- **Difficulty:** medium
- **Location:** `src/search/router.rs:960-975` (`try_classify_negation`); token list `src/search/router.rs:377-388`
- **Description:** The negation classifier sits at priority 1 — above identifier lookup, cross-language, type-filtered, and structural — and fires on any whitespace-split token that hits the set `{not, without, except, never, avoid, no, don't, doesn't, shouldn't, exclude}`. The set includes plain English particles that appear inside completely non-negation queries: `"no"` is a single-token answer or a placeholder; `"exclude"` and `"avoid"` appear in identifier names or doc-string-style queries (`"exclude_test_files function"`, `"avoid contention"`); `"except"` is a Python keyword. Concrete misroute: `cqs "exclude_test pattern"` → tokens `["exclude_test", "pattern"]` → no hit (because the token is `exclude_test` not `exclude`), but `cqs "exclude tests"` → tokens `["exclude", "tests"]` → hits Negation, routes to `DenseBase` with `α=Negation`'s value, and the operator's "find code that excludes tests" intent is treated as "find tests, then negate" against the wrong index. The `try_classify_*` refactor in #1509 made each classifier independently testable but didn't add a context check.
- **Suggested fix:** Two-arm gate: only fire if a negation token appears AND there are ≥2 tokens after it OR there's a non-negation keyword before it. Equivalently: require the negation token to function as a connective (e.g., `query.contains(" without ")`, `query.contains(" except ")`) rather than appearing alone or at the end. Add adversarial tests `classify("exclude tests")`, `classify("avoid lock contention")`, `classify("no panic")` asserting they DON'T classify as Negation.

---

#### AC-V1.38-5: `resolve_splade_alpha` global-env arm silently drops parse errors / non-finite values that the per-cat arm warns about
- **Difficulty:** easy
- **Location:** `src/search/router.rs:783-796` (global env match); compare with per-cat arm at lines 745-781
- **Description:** The per-category SPLADE α env (`CQS_SPLADE_ALPHA_<CAT>`) match has three explicit arms: `Ok(val) if let Ok(alpha)` with non-finite warning, `Ok(val)` with parse-error warning, and explicit `Err(NotPresent)` / `Err(NotUnicode)` arms (EH-V1.36-9 added the latter). The global `CQS_SPLADE_ALPHA` arm collapsed all of those into `_ => {}`: a malformed `CQS_SPLADE_ALPHA=NaN`, `CQS_SPLADE_ALPHA=foo`, or non-unicode value falls through to slot/default with **no warning at all**. Operator who typoed `CQS_SPLADE_ALPHA=O.7` (capital O) gets the per-cat default silently and chases an A/B that doesn't reflect the env they thought was active. This isn't a silent corruption (the default is reasonable) but it's an observability gap masquerading as algorithm correctness — the algorithm "fall back to default on bad input" is the same in both arms, but only the per-cat arm tells you it happened.
- **Suggested fix:** Mirror the per-cat arm structure: explicit `Ok(val)` with parse-fallback warning, explicit `Err(VarError::NotUnicode)` with warning, `Err(VarError::NotPresent)` silent. Or factor both into a shared helper since the parse-clamp-warn-return pattern is otherwise identical.

---

#### AC-V1.38-6: `apply_parent_boost` cap clamp can overshoot `parent_boost_cap` by ~1 ULP when `(cap - 1.0) / per_child` is not exact in f32
- **Difficulty:** easy
- **Location:** `src/search/scoring/candidate.rs:75-77,103-106`
- **Description:** AC-V1.25-4 closed this once by changing `count` clamping. The current implementation computes `max_children = (cfg.parent_boost_cap - 1.0) / cfg.parent_boost_per_child` and then `boost = 1.0 + per_child * (count as f32 - 1.0).min(max_children)`. For most config values the round-trip is exact (e.g. cap=1.15, per=0.05 → max_children=3.0 by chance), but operator-overrides like `parent_boost_cap = 1.20, parent_boost_per_child = 0.03` produce `max_children = 0.20/0.03 ≈ 6.6666665`; the multiplied-back value `0.03 * 6.6666665 ≈ 0.2000` is within ULP of 0.2 but can overshoot. With count ≥ max_children+1, the resulting boost is `1.0 + 0.03 * 6.6666665 ≈ 1.2000000476...` — strictly greater than the documented cap. Sort-stability tests don't catch this because both pre- and post-boost scores are still well-ordered; the residual is a documentation/contract violation.
- **Suggested fix:** One extra clamp on the final value: `let boost = (1.0 + per_child * ...).min(cfg.parent_boost_cap);`. Costs one f32.min, fixes the ULP overshoot, and matches what the doc string at `candidate.rs:50-52` already promises ("capped at `parent_boost_cap`").

---

#### AC-V1.38-7: `from_preset(unknown_name)` silently falls back to default model — `cqs index --model typo` against an existing index runs to completion against the wrong model with no warning
- **Difficulty:** medium
- **Location:** `src/embedder/models.rs:643-662` (`ModelConfig::resolve`); interaction with drift check at `src/cli/commands/index/build.rs:1497-1525` (`check_index_model_drift`)
- **Description:** PR #1505 added `check_index_model_drift` to catch the silent dim-mismatch footgun where `cqs index --model X` against a Y-built index would feed X-dim vectors into a Y-dim store. But `ModelConfig::resolve` short-circuits unknown CLI model names: `if let Some(cfg) = Self::from_preset(name) { return cfg; } tracing::warn!(...); return Self::default_model();`. So `cqs index --model bgelarge` (typo, missing dash) emits a `tracing::warn!` (rarely visible at the operator's default log level) and resolves to the project default, which is `bge-large`. If the existing index is also `bge-large`, the drift check passes — operator believes they switched models, the index keeps building against the original. Worse case: the index is `embeddinggemma-300m`, the operator typoed `--model bge-large-en` (similar but not the preset name `bge-large`), default is `bge-large` (768-dim vs gemma's 768-dim — same dim, different vocab) → drift check fires correctly. But if defaults align with the existing index, the typo is invisible.
- **Suggested fix:** Either (a) make `cqs index --model X` hard-fail on unknown preset (don't fall back to default — the operator typed an explicit value, honour it as a request not a hint), or (b) elevate the existing `tracing::warn!` to an `eprintln!` so the typo is visible regardless of log filter, plus add an `unknown_preset = true` field to the warn and assert in `check_index_model_drift` that no unknown-preset fallback happened. The existing fallback-to-default behaviour is the safe choice for `cqs <query>` and similar read paths, but `cqs index --model` is an operator-driven write path where silence is wrong.

---

#### AC-V1.38-8: `is_vendored_origin` config entries with slashes silently never match — operator override gets ignored without warning
- **Difficulty:** easy
- **Location:** `src/vendored.rs:53-60`; config consumer in `src/cli/commands/index/build.rs:438-443`
- **Description:** `is_vendored_origin` matches each forward-slash-separated segment of the origin path against the prefix list with strict `==`. The doc says "entries should be bare directory names without slashes (the default list satisfies that contract)" — but `effective_prefixes` happily accepts whatever an operator puts in `.cqs.toml`'s `[index].vendored_paths`. If the operator writes `vendored_paths = ["vendor/oss-lib", "third_party/protobuf"]` expecting sub-path matching (a perfectly reasonable mental model — the config name is "vendored *paths*", not "vendored *segments*"), every entry with a slash in it is dead config: a single segment `"vendor"` from `vendor/oss-lib/foo.rs` will never `==` the multi-segment string `"vendor/oss-lib"`. No validation, no warning at load — vendor-tagging just silently fails for those overrides.
- **Suggested fix:** At `effective_prefixes` resolution time, validate each override entry with `if entry.contains('/') { tracing::warn!(?entry, "vendored_paths entry contains '/' and will never match — use a bare directory segment"); }` and either drop the entry or accept it but log the no-op. Even better: support multi-segment entries by checking if the origin contains `/{entry}/` or starts with `{entry}/` — both modes are useful and the function name doesn't constrain to either.

---

#### AC-V1.38-9: HNSW `ef_search` integer overflow when `k * 2` exceeds usize bounds
- **Difficulty:** easy
- **Location:** `src/hnsw/search.rs:93-94`
- **Description:** `let ef_search = self.ef_search.max(k * 2).min(index_size);` — `k * 2` is unchecked. On 64-bit usize, `k > usize::MAX / 2` will panic (unsigned overflow on `k * 2` is undefined in release builds without `-C overflow-checks`, panics in debug). Realistically the CLI bounds k via `--limit` parsing, but `search_filtered` is a public API on `HnswIndex` and callers are not contractually obligated to bound k. A daemon client request that smuggled in `k = usize::MAX - 1` would overflow. The HNSW author's defensive `.min(index_size)` covers the underlying library's k-vs-size sanity but not the intermediate computation. `saturating_mul` is the obvious patch and matches what `cagra.rs:751` does for `chunk_count.saturating_mul(dim)`.
- **Suggested fix:** `let ef_search = self.ef_search.max(k.saturating_mul(2)).min(index_size);`. Same one-line change in `cagra.rs:448` (`(k * 2).clamp(itopk_min, itopk_max).max(k)` — `k * 2` here has the same overflow shape).

---

#### AC-V1.38-10: `cmd_index --model X` drift check passes for unknown model even when the preset registry would silently substitute the default
- **Difficulty:** medium
- **Location:** `src/cli/commands/index/build.rs:340-358` + `1497-1525` (`check_index_model_drift`)
- **Description:** Companion to AC-V1.38-7 from the drift-check angle. The drift-check runs *after* `cli.try_model_config()?` — but `try_model_config` reads the resolved `ModelConfig`, which already silently fell back to default for unknown CLI inputs (see AC-V1.38-7). So the drift check has no way to tell the difference between "operator explicitly asked for the same model that's already on disk" and "operator typoed an unknown preset and got the default which happens to match what's on disk". Only the `tracing::warn!` in `ModelConfig::resolve` records the discrepancy, and the drift check doesn't read it. End result: drift check is a sentinel against the dim-mismatch footgun (correct behaviour for that scope), but it's *not* the operator-misconfiguration sentinel its prose suggests. Combined with the asymmetric repo-vs-name match documented in PR #1505's body, the test surface looks comprehensive but only covers the dim-mismatch case.
- **Suggested fix:** Plumb the unknown-preset signal back to the call site: `ModelConfig::resolve` returns `Result<Self, ResolveErr>` (or an additional bool field on the struct, or a separate helper `try_resolve_strict`), and the build path uses the strict variant when it's an explicit `--model` (vs an env/config-file resolution). Pair with AC-V1.38-7's fix so the two findings collapse into a single behavioural change.

---

## Extensibility

# Extensibility Audit — post-v1.38.0

Audit run 2026-05-06 against main (post v1.38.0 / pre-next). Audit mode ON.
Skips findings already closed by #1474 / #1482 / #1483 / #1500 / #1495 /
#1508 / #1509 / #1510 / #1511.

#### EX-V1.38-1: `doc_format_for` dispatches by string-tag through 12-arm match — adding a doc style is a 2-place edit
- **Difficulty:** easy
- **Location:** `src/doc_writer/formats.rs:50-130` + `src/language/mod.rs:392` (`pub doc_format: &'static str`)
- **Description:** `LanguageDef.doc_format` carries a string tag (`"javadoc"`, `"triple_slash"`, `"python_docstring"`, …) that `doc_format_from_tag()` matches against to produce a `DocFormat` literal. Adding a new comment style (e.g. Zig's `///`-with-`!` doc, Nim's `##`, Tcl's `#`-aligned) requires both: a row in the giant `match tag` *and* the right tag string in `LANG_FOO`. Mistype the tag and the language silently falls into the `_ =>` default (Java-ish `// `). 13 languages opted into `"javadoc"`, so the bulk-rename cost is also non-trivial.
- **Suggested fix:** Replace `pub doc_format: &'static str` with `pub doc_format: DocFormat` (or `&'static DocFormat`). Move the 13 `DocFormat { … }` literals next to each `LANG_FOO` definition (or behind named statics like `DOC_FORMAT_JAVADOC` for de-duplication). Delete `doc_format_from_tag` and the tag string entirely. Compiler enforces full population, no silent fallback.

#### EX-V1.38-2: C# / Java / Kotlin / Python `post_process_chunk` re-hardcodes `[Test]` / `@GetMapping` lists already on `LanguageDef::test_markers`
- **Difficulty:** medium
- **Location:** `src/language/languages.rs:581-595` (csharp); analogous blocks in java / kotlin / python post-processors
- **Description:** `LanguageDef.test_markers` exists and is used for indexing analytics (`all_test_markers`), but the `post_process_chunk_*` functions that promote `Function → Test` / `Function → Endpoint` carry their **own** parallel hardcoded `header.contains("[Test]")` / `header.contains("@Test")` / `header.contains("[HttpGet]")` checklists. So adding xUnit's `[Theory(...)]` or Spring's `@RestController`-derived endpoints means editing two unrelated spots — and the table for `Endpoint` markers (`[HttpGet]`/`@GetMapping`/`@RequestMapping`/`@app.route`/`@router.get`) has no `LanguageDef` field at all.
- **Suggested fix:** (1) Make `post_process_chunk_*` consult `lang.def().test_markers` instead of inline literals — the existing field is the source of truth. (2) Add `pub endpoint_markers: &'static [&'static str]` on `LanguageDef`; populate per-language. (3) Replace the inline `header.contains("[HttpGet]") || …` chains with `endpoint_markers.iter().any(|m| header.contains(m))`.

#### EX-V1.38-3: Reranker tunables (`batch`, `max_length`, `pool_max`, `over_retrieval`) are env-only — no `[reranker]` config knobs
- **Difficulty:** easy
- **Location:** `src/reranker.rs:92` (CQS_RERANKER_BATCH), `src/reranker.rs:264` (CQS_RERANKER_MAX_LENGTH), `src/cli/limits.rs:62-73` (CQS_RERANK_OVER_RETRIEVAL / CQS_RERANK_POOL_MAX)
- **Description:** `[reranker]` table in `.cqs.toml` (`AuxModelSection`) only has `preset` / `model_path` / `tokenizer_path`. Operators tuning rerank perf for a slow model can only set 4 env vars. New `[index.policy]` (#1511) is the right precedent — same gap exists for reranker. Same for the SPLADE-side knobs (CQS_SPLADE_ALPHA, used heavily in eval sweeps per memory).
- **Suggested fix:** Promote to `AuxModelSection`: optional `batch`, `max_length`, `pool_max`, `over_retrieval`. Keep env vars as override-on-top precedence (matches existing CLI → env → TOML chain). Add `[reranker.policy]` sub-table if you want symmetry with `[index.policy]`.

#### EX-V1.38-4: Classifier vocab overlay covers only 2 of 6 vocabularies
- **Difficulty:** medium
- **Location:** `src/search/router.rs:303-363` (BEHAVIORAL_VERBS, CONCEPTUAL_NOUNS, STRUCTURAL_KEYWORDS — all `const`); `src/search/router.rs:519` (`install_classifier_vocab_overlay` only handles negation + multistep)
- **Description:** #1483 added a `classifier.toml` overlay for `negation_tokens` and `multistep_patterns`, but the other four vocabularies the classifier consults — `BEHAVIORAL_VERBS` (28 entries), `CONCEPTUAL_NOUNS` (14 entries), `STRUCTURAL_KEYWORDS`, and the implicit NL_INDICATORS — remain compile-time constants. A user wanting to teach the router that "orchestrates" is behavioral, or that "topology" is conceptual, has no recourse short of a fork. The overlay design is already there; finishing it is mechanical.
- **Suggested fix:** Mirror the `NEGATION_TOKENS` `LazyLock<RwLock<HashSet>>` / `MULTISTEP_PATTERNS_AC` `LazyLock<RwLock<Arc<AhoCorasick>>>` pattern for the remaining four sets. Extend `load_classifier_vocab_overlay` schema (`behavioral_verbs`, `conceptual_nouns`, `structural_keywords`, `nl_indicators`). Same TOML, same install function.

#### EX-V1.38-5: `cqs task` waterfall budget weights are `const f64` — no operator knob
- **Difficulty:** easy
- **Location:** `src/cli/commands/train/task.rs:268-275`
- **Description:** WATERFALL_SCOUT/CODE/IMPACT/PLACEMENT (0.15/0.50/0.15/0.10) decide how `--max-tokens` is divided across sections of `cqs task` output. Operators and agents have very different preferences (an agent doing impact analysis wants more `impact` budget). Right now the only path is recompile.
- **Suggested fix:** Promote to a new `[task]` section in config (or extend `ScoringOverrides` knob registry — same pattern: one row, no schema churn). At minimum, add env vars `CQS_TASK_WATERFALL_{SCOUT,CODE,IMPACT,PLACEMENT}` matching the existing `parse_env_*` pattern in `src/limits.rs`.

#### EX-V1.38-6: `extract_calls_from_chunk` has a hardcoded `Language::Markdown` branch — should route through `custom_call_parser`
- **Difficulty:** easy
- **Location:** `src/parser/calls.rs:124-137`
- **Description:** The exact code smell `LanguageDef::custom_call_parser` was meant to eliminate: `if chunk.language == Language::Markdown { return crate::parser::markdown::extract_calls_from_markdown_chunk(chunk); }`. Adding another grammar-less language with custom call extraction (e.g. SQL stored-proc cross-refs, L5X tag references, or a future natural-language doc format) requires editing this site instead of just populating `def.custom_call_parser`. Note the related dispatch in `src/parser/mod.rs:516-548` — grammar-less languages without `custom_all_parser` *silently fall through to the markdown path*, which is also surprising.
- **Suggested fix:** Add a `chunk_call_parser: Option<fn(&Chunk) -> Vec<CallSite>>` field on `LanguageDef` and wire `extract_calls_from_chunk` to consult it before the tree-sitter path. Markdown registers `extract_calls_from_markdown_chunk`. Then the `Language::Markdown` literal goes away.

#### EX-V1.38-7: `Language::Python` docstring extraction lives inside `extract_doc_comment`, not on `LanguageDef`
- **Difficulty:** medium
- **Location:** `src/parser/chunk.rs:251-263`
- **Description:** Python is the only language that places doc comments **inside** the function body (`def f(): """docstring"""`) rather than as preceding sibling comments. The fallback path in `extract_doc_comment` carries an `if language == Language::Python { … }` block that walks `body → expression_statement → string`. Any other "docstring-style" language (a hypothetical Lua via LDoc-as-first-string, or Python-syntax DSLs in `.cqs` query files) needs to be hand-stitched here. `InsertionPosition::InsideBody` already exists in `DocFormat` — symmetry would say there's an `extract_inside_body_doc` hook.
- **Suggested fix:** Add `pub inside_body_doc_extractor: Option<fn(node, source) -> Option<String>>` (or a `DocPlacement` enum: `BeforeAsSibling | InsideBodyAsString { kind: &'static str }`). Move the Python branch into the python `LanguageDef` populator. Removes the `Language::Python ==` literal from chunk.rs.

#### EX-V1.38-8: `doc_writer/formats.rs` has Go-specific "prepend FuncName to first line" rule hardcoded with `if language == Language::Go`
- **Difficulty:** easy
- **Location:** `src/doc_writer/formats.rs:158-170`
- **Description:** Go's `// FuncName does X` convention is encoded as a literal `if language == Language::Go { /* prepend func name */ }` in the formatter. Other languages that want subject-first conventions (e.g. Erlang `%% function/arity:`, Elixir doc that wants `@doc "function/arity ..."`, or a custom house style) can't opt in without code edits.
- **Suggested fix:** Add a `doc_first_line_template: Option<&'static str>` (e.g. `"{name} "` for Go, `None` for everyone else) to `DocFormat` (or as a sibling field on `LanguageDef`). Formatter does template substitution if `Some`. Removes the `Language::Go` literal.

#### EX-V1.38-9: `extract_method_receiver_type` has `if language != Language::Go { return None; }` — Go-specific receiver logic should be on `LanguageDef`
- **Difficulty:** medium
- **Location:** `src/parser/chunk.rs:611-624`
- **Description:** Go is the only language whose method-receiver type isn't on the parent container — it's on the method node itself (`func (r *Server) Handle()`). The current code guards with `language != Language::Go { return None; }`. Rust trait impls, Swift extensions, Objective-C categories, and C++ out-of-class member definitions all have similar "look-elsewhere for the container" needs that today either don't work or fall back to surrounding-container heuristics.
- **Suggested fix:** Add `pub receiver_type_extractor: Option<fn(node, source) -> Option<String>>` on `LanguageDef`. Go populates it with the existing function. The `language != Language::Go` literal goes away and other languages can opt in.

#### EX-V1.38-10: `test_type_queries_compile` hand-lists the 11 languages with type queries instead of iterating `Language::ALL`
- **Difficulty:** easy
- **Location:** `src/parser/calls.rs:1096-1121`
- **Description:** Test harness for "all type queries compile" enumerates `Language::Rust, TypeScript, Python, Go, Java, C, CSharp, Scala, Cpp, Php, Zig` — adding a 12th language with a `type_query` won't be tested unless someone remembers this list. The exhaustive-iteration version is `Language::ALL.iter().filter(|l| l.def().type_query.is_some())`.
- **Suggested fix:** Replace the array with `for lang in Language::ALL.iter().copied().filter(|l| l.def().type_query.is_some())`. Bonus: add a sibling test that asserts `try_def().is_some()` for every `type_query.is_some()` language so disabled-feature combinations don't silently skip the query.

---

## Platform Behavior

## Platform Behavior — post-v1.36.2 (v1.38.0 + post-v1.38 work)

Scope: NEW sites added since the v1.36.2 audit. Re-confirmed `audit-findings.md` lines 673-733
already cover: SEC-1 audit-mode/project.rs Windows ACL gap (P4-19), `db_file_identity` Windows
mtime fallback (P4-17), `tasklist` UTF-16 BOM (P4-10 — fixed in #1490), daemon `cfg(unix)` fence
(P4-9 / #1512), `lookup_main_cqs_dir` non-canonicalization, `strip_prefix` leakage. Skipped.

---

#### PL-V1.38-1: CAGRA `.cagra` blob is born world-readable on Unix — SEC-1 contract gap vs HNSW
- **Difficulty:** easy
- **Location:** `src/cagra.rs:1341` (`gpu.index.serialize(...)` → tmp file written by cuVS), promoted via `crate::fs::atomic_replace` at line 1385 with no chmod
- **Description:** `save_blob_atomic_with_rollback` lets cuVS create the `.cagra.tmp` file via FFI; cuVS uses default umask (typically 0o644). Unlike `hnsw/persist.rs:458-470` which explicitly `set_permissions(0o600)` on `.hnsw.graph` / `.hnsw.data` AFTER cuVS-equivalent (here `hnsw_rs`) writes them and BEFORE rename, CAGRA's save path skips the chmod entirely. The promoted `index.cagra` ends up world-readable on multi-user Linux; the `.bak` rollback file (created by `rename` at line 1357) inherits the same loose mode. Anyone in the `cqs` group can read another user's vector index — which contains the embedded chunk content effectively (graph topology + dim + len). Same SEC-1 promise SECURITY.md / cache.rs / store.rs / hnsw/persist.rs all enforce; CAGRA is the lone exception. Not flagged in v1.36.2 because the `.bak` rollback work (#1492) was post-audit.
- **Suggested fix:** After line 1414 (successful `atomic_replace`) and again after `bak_path` rename at 1357, `#[cfg(unix)] std::fs::set_permissions(path, Permissions::from_mode(0o600))` and the same on `bak_path` while it briefly exists. Alternatively, gate before serialize by setting umask 0o077 around the `gpu.index.serialize` call (the cache.rs:295-420 SEC-V1.33-2 pattern). Same fix needed for `cagra.rs:1453` (`write_meta_atomic` `File::create` for the JSON sidecar — `id_map` field at line 877 leaks every chunk_id, which embeds filenames + line ranges).

#### PL-V1.38-2: SPLADE `splade.index.bin.bak` inherits umask on Windows + leaks 0o600 on cross-device fallback
- **Difficulty:** easy
- **Location:** `src/splade/index.rs:483` (`std::fs::rename(path, &bak_path)`), `src/fs.rs:55` (`atomic_replace` cross-device `std::fs::copy` fallback)
- **Description:** Two related gaps in the new `.bak` rollback path landed in #1491. (1) The live `splade.index.bin` is correctly born `0o600` on Unix via `OpenOptionsExt::mode(0o600)` at line 391, but on Windows the `cfg(not(unix))` branch at line 396 falls back to `File::create` with no ACL hardening — same shape as the audit-mode finding (P4-19), but for SPLADE which is a new site (#1491). (2) When the live file is renamed to `.bak` at line 483 and then `atomic_replace` (`fs.rs:51-85`) hits a cross-device error (WSL `/mnt/c/`, NFS, overlayfs), the fallback path uses `std::fs::copy(tmp_path, &dest_tmp)` which on Windows does NOT preserve the source ACL (CopyFileW skips DACL by default), and on Unix only preserves source mode if `tmp_path` actually has restrictive perms — the doc-comment at fs.rs:27-29 promises "the rename preserves them on unix" but the cross-device branch silently breaks that contract. So a hardened `.tmp` ends up with default ACL on Windows after the EXDEV fallback fires.
- **Suggested fix:** (1) Add a Windows ACL fixup branch at splade/index.rs:394 mirroring whatever the umbrella P4-19 fix lands on. (2) In `fs.rs::atomic_replace` cross-device branch at line 55, after `std::fs::copy`, re-apply the source's permissions explicitly (`std::fs::set_permissions(&dest_tmp, std::fs::metadata(tmp_path)?.permissions())`); update the doc-comment at line 27-29 to be honest about the Windows-cross-device case.

#### PL-V1.38-3: `is_suspicious_cache_path` doc-comment promises Windows checks the impl skips
- **Difficulty:** easy
- **Location:** `src/aux_model.rs:115-117` (doc) vs `src/aux_model.rs:139-150` (impl)
- **Description:** Doc-comment at line 115-117 says "World-writable or guest-shared dirs: `/tmp`, `/var/tmp`, `/dev/shm` (Linux); `%TEMP%`, `%TMP%` (Windows)." The implementation only checks the three Linux paths as hardcoded string literals. On Windows, a hostile `HF_HOME=C:\Windows\Temp\hf` or `HF_HOME=C:\Users\Public\hf` flies straight through `is_suspicious_cache_path` and gets returned to the embedder loader — the SEC-V1.33-8 / #1339 supply-chain protection is silently no-op on Windows. Same shape as the existing "docs lying is P1" rule. Also misses `std::env::temp_dir()` on every platform (a custom `$TMPDIR=/var/run/...` setup on Linux escapes the check the same way).
- **Suggested fix:** After the hardcoded prefix loop at line 139-150, `if let Some(t) = std::env::temp_dir().to_str() { if path.starts_with(t) { return Some("under platform temp_dir"); } }`. On `cfg(windows)`, also check `std::env::var_os("PUBLIC")` (typically `C:\Users\Public`) and `std::env::var_os("ProgramData")`. Or rewrite the doc to match what the code does.

#### PL-V1.38-4: Two divergent WSL-DrvFS detectors disagree on custom `automount.root`
- **Difficulty:** medium
- **Location:** `src/config.rs:107-124` (`is_wsl_drvfs_path`) vs `src/cli/watch/mod.rs:289-294` (`is_under_wsl_automount` + cached parse of `/etc/wsl.conf`)
- **Description:** The watch helper reads `/etc/wsl.conf` for `[automount] root=/win/` and returns true for `/win/c/...`, correctly triggering `--poll`. But `coarse_fs_resolution` (config.rs:157-162) calls `is_wsl_drvfs_path` which is hard-coded to `/mnt/<letter>/` + `//wsl.localhost/` + `//wsl$/` — it never reads wsl.conf. Result: on a WSL host with `automount.root=/win/` and a project at `/win/c/Projects/foo`, the watch loop polls (good) but `coarse_fs_resolution` returns 0 (Linux ext4-equivalent) instead of 2s — and `events.rs::collect_events` mtime-skip uses strict-equality on a 2s-granular FS, silently dropping every rapid re-save. The bug class PB-V1.30.1-5 / #1225 was supposed to close. Same data living in two places diverged when only one helper learned about wsl.conf.
- **Suggested fix:** Promote `parse_wsl_automount_root` from cli/watch/mod.rs to `cqs::config` (or a new `cqs::wsl` module) and have `is_wsl_drvfs_path` consult it via the same `OnceLock`. Both call sites then share a single source of truth. Keep the `is_wsl_drvfs_path` signature (it stays a `&Path` predicate) so the watch detector becomes a thin wrapper.

#### PL-V1.38-5: `cqs init` writes a `.gitignore` missing 11 of the 14 files `.cqs/` actually contains
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/init.rs:44-47`
- **Description:** The init helper writes a 9-entry gitignore (`index.db`, `index.db-wal`, `index.db-shm`, `index.lock`, `index.hnsw.{graph,data,ids,checksum,lock}`, `*.tmp`). A real cqs project rooted in this repo's `.cqs/` actually has: `audit-mode.json`, `embeddings_cache.db`, `index.cagra`, `index.cagra.meta`, `index.cagra.bak`, `index_base.hnsw.*`, `splade.index.bin`, `splade.index.bin.bak`, `slots/`, `slots.lock`, `store.db`, `telemetry*.jsonl`, `telemetry.lock`, `active_slot` — **none** are gitignored. A user runs `cqs init` then `git add .` and commits ~hundreds of MB of binary index data, including `audit-mode.json` (deliberately marked SEC-1) and `telemetry*.jsonl` (operator hostnames, exec timing). Cross-platform issue. Slot/CAGRA/SPLADE/cache/audit/telemetry files were all added after the gitignore template was authored and never updated.
- **Suggested fix:** Replace the literal string with a file-list source-of-truth. Either (a) generate the gitignore from a `const SLICE: &[&str] = &[...]` that other code can also iterate, or (b) write `*\n!.gitignore\n` (gitignore-everything-but-itself) — this is the simpler solution and survives all future additions. The CRLF-vs-LF split at lines 44-47 also flaps on macOS users with `core.autocrlf=true`; gate on `is_wsl()` or document why only Windows-native gets CRLF.

#### PL-V1.38-6: `daemon_control_hint` returns systemctl on linux + pkill on macOS — silently wrong on FreeBSD/OpenBSD/illumos
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/model.rs:75-112`
- **Description:** The three-arm cfg matches `linux`, `macos`, and a generic `not(any(linux, macos))` fallback. The fallback returns "stop the cqs watch process" prose, which is correct *prose* but technically wrong on FreeBSD/OpenBSD/NetBSD/illumos — `daemon_translate.rs` and the watch socket path are `cfg(unix)`, so the daemon DOES build and run on these systems, and the operator gets a useless hint instead of `pkill -TERM -f 'cqs watch --serve'` which works identically to the macOS branch. Low-priority because cqs's officially-supported targets are Linux/macOS/Windows per CHANGELOG, but the README says "POSIX-compatible" — and the fallback contradicts that.
- **Suggested fix:** Change the gates from `cfg(target_os = "linux")` / `cfg(target_os = "macos")` / `cfg(not(...))` to `cfg(target_os = "linux")` / `cfg(unix)` / `cfg(not(unix))`. The macOS branch's `pkill` invocation works on every BSD and illumos. Same fix in `stop_daemon_best_effort` at line 649-704.

#### PL-V1.38-7: SPLADE / CAGRA tmp-file `to_string_lossy()` fallback can collide across concurrent saves on non-UTF-8 paths
- **Difficulty:** medium
- **Location:** `src/splade/index.rs:369-376`, `src/cagra.rs:1310-1315` and `src/cagra.rs:1445-1450`
- **Description:** All three sites build a tmp filename from `path.file_name().to_string_lossy()` and join with `temp_suffix()` to disambiguate. The doc-comment at splade/index.rs:365-368 specifically calls out the `to_str().unwrap_or(default)` collision risk and switches to `to_string_lossy()` — but `to_string_lossy()` ALSO collapses on non-UTF-8 input, replacing every invalid byte with `U+FFFD` (`\u{FFFD}`). Two concurrent saves to `splade.index_\xff_a.bin` and `splade.index_\xff_b.bin` get the same `to_string_lossy` output and only `temp_suffix()` (a 64-bit random) saves us — collision probability is low but nonzero, and the random `temp_suffix` fact isn't documented in the splade comment which talks only about `to_string_lossy`. CAGRA has the worse shape: line 1313 falls back to `"index.cagra"` and line 1448 falls back to `"cagra_meta"` on non-UTF-8 — both shared across all concurrent saves of any non-UTF-8 path. On Linux/macOS, file names CAN legally be non-UTF-8 (NTFS-mounted volumes via WSL, FUSE, archived/extracted tarballs from non-UTF-8 locales). Test fixtures only ever use ASCII, so this never trips locally.
- **Suggested fix:** Build the tmp basename from `as_encoded_bytes()` + hex-encode (or `OsStr` round-trip), guaranteeing a unique 1:1 mapping per source path. Or — cheaper — use `temp_suffix()` ALONE without the `file_name`-based prefix; the temp file is in the same dir as the live file and gets renamed away within microseconds, the prefix is mostly cosmetic.

#### PL-V1.38-8: `train_data::git::Command::new("git")` has the same PATH-lookup gap PB-V1.33-10 fixed for `tasklist`
- **Difficulty:** medium
- **Location:** `src/train_data/git.rs:77, 167, 316, 389, 419, 428, 434, 445, 451, 473, 479` and `src/train_data/mod.rs:537`
- **Description:** Every `train_data` invocation does `Command::new("git")` and relies on PATH lookup. PB-V1.33-10 / #1463 / #1490 just landed the fix for `tasklist` (resolve absolute path from `%SystemRoot%\System32`) precisely because a stripped-PATH context (Docker, GHA Windows runner with custom PATH, systemd unit with `Environment=PATH=...`) silently fails with `ErrorKind::NotFound`. The `train_data` flow is CLI-only and the operator running `cqs train ...` likely has git on PATH today, but the cqs daemon could (in a future Windows port — #1512) trigger train-data work, and a service-account daemon often has a stripped PATH. Same root cause; not a runtime crash but a silent "no commits found" that the operator can't diagnose.
- **Suggested fix:** Either (a) cache a one-time `which::which("git").context("git not on PATH; required for train-data extraction")?` at process start and pass `&Path` through; or (b) add a `resolve_git_path()` helper mirroring `cli/files.rs::process_exists` Windows shape — `%ProgramFiles%\Git\bin\git.exe` on Windows, `/usr/bin/git` then `/usr/local/bin/git` on Unix. (b) is more in keeping with the PB-V1.33-10 pattern. Lower priority than the cli/files.rs `tasklist` case because `git` on PATH is reasonable to assume in most train-data contexts.

#### PL-V1.38-9: `coarse_fs_resolution` returns Duration::ZERO on Windows — FAT32/exFAT mounts silently drop rapid saves
- **Difficulty:** medium
- **Location:** `src/config.rs:157-185` (function body), `src/config.rs:173-181` (Windows `else` branch)
- **Description:** `coarse_fs_resolution` returns 2s for WSL DrvFS, calls `linux_fs_resolution` / `macos_fs_resolution` to detect FAT/HFS/SMB/NFS via statfs magic numbers, but on Windows native (the `cfg(not(any(linux, macos)))` branch at 173-181) returns `None` → `Duration::ZERO`. That's correct for NTFS (100ns granularity) but Windows users CAN have a project on a USB FAT32 drive, an SD card with exFAT, or a network SMB share — all of which have 2s mtime granularity. The watch loop's mtime-equality skip then silently drops every rapid second-save, exactly the bug class PB-V1.30.1-5 / #1225 was meant to close on Linux/macOS. cqs runs on Windows per CHANGELOG; native Windows isn't in the v1.36.2 audit's PB scope but this is a fresh "narrow cfg gate" pattern.
- **Suggested fix:** Add a `windows_fs_resolution` shim that calls `GetVolumeInformationW` on the path's volume root and reads `lpFileSystemNameBuffer`; map "FAT" / "FAT32" / "exFAT" / "CDFS" / "UDF" to 2s, "NTFS" to 0. The `windows-sys` crate already pulls in the necessary bindings (used elsewhere). Or punt and return 1s globally on Windows — slight overshoot but correct on every FS the user could mount.

#### PL-V1.38-10: `aux_model::is_path_like` accepts paths but `dirs::cache_dir()` differs Windows-vs-WSL — same string is "cached" or "outside home" depending on cqs.exe vs cqs (Linux)
- **Difficulty:** medium
- **Location:** `src/aux_model.rs:163-168` (`in_cache = dirs::cache_dir().is_some_and(...)`)
- **Description:** `is_suspicious_cache_path` flags paths "outside user's home + system cache dir". On WSL Linux, `dirs::cache_dir()` returns `~/.cache/` (Linux XDG); on Windows-native it returns `%LOCALAPPDATA%`; on WSL with `wsl --windows-host` interop, neither helps for paths like `/mnt/c/Users/foo/AppData/Local/`. Result: the same `HF_HOME=/mnt/c/Users/foo/AppData/Local/huggingface` flagged "outside home + cache" by WSL cqs (Linux build) is ACCEPTED by Windows cqs.exe — same path, same operator, opposite outcomes. Operator confusion + a real protection gap when running cqs across platforms on shared paths. Same path-handling skew as the existing `lookup_main_cqs_dir` finding (audit-findings.md:693) but in the SEC-V1.33-8 supply-chain check.
- **Suggested fix:** When the path lives under a WSL-known Windows mount (`is_wsl_drvfs_path`), translate to the Windows-side cache dir for the comparison: `/mnt/c/Users/foo/AppData/Local` → equivalent of `%LOCALAPPDATA%`. Or document that `CQS_HF_CACHE_TRUSTED=1` is required when crossing the WSL/Windows boundary. The doc-comment at line 121-123 already mentions `%LOCALAPPDATA%` — make the impl honor it under WSL.

---

### Summary

10 platform findings, all confirming patterns already audited in v1.33 / v1.36.2 but at NEW
sites added in the v1.36-v1.38 window. The dominant class is "new persistence path landed
without the SEC-1 0o600/ACL discipline that older paths enforce" (PL-V1.38-1, -2, -5).
Two findings are docs-vs-code lies (PL-V1.38-3, -10 — `is_suspicious_cache_path` claims
Windows coverage it never had). Two are duplicate-source-of-truth divergences that broke
when only one copy got updated (PL-V1.38-4 WSL detector, PL-V1.38-5 gitignore template).
PL-V1.38-1 (CAGRA chmod) and PL-V1.38-5 (gitignore) are the highest-impact: a default
`cqs init && git add .` commits the `audit-mode.json` SEC-1 file, and CAGRA's loose mode
contradicts the SECURITY.md promise on multi-user Linux. The rest are latent and tracked
under the existing P4-9 / #1512 Windows-port umbrella, surfaced here so the umbrella PR
covers them in one pass instead of leaving "PL-V1.39-* one more place to fix" for later.

---

## Security

# Security Audit — v1.38.0 + post-v1.38 work

Audit performed against `main` @ `fe7126c4` (post-#1505/#1506).
Audit mode ON. Cross-referenced v1.36.2 SEC findings to avoid re-filing.

#### SEC-V1.38-1: `llm_config.api_base` logged with userinfo intact at info / debug level
- **Difficulty:** easy
- **Location:** `src/llm/local.rs:128-135, 164-171, 376-380`; `src/llm/hyde.rs:21-25`; `src/llm/summary.rs:27-31`; `src/llm/doc_comments.rs:144-148`
- **Description:** `LlmConfig::resolve` (`src/llm/mod.rs:280-323`) only synthesizes a redacted `safe_url` for the http/https warning path; it never strips userinfo from the stored `api_base` field. Five downstream sites then log the *raw* `api_base` via `%llm_config.api_base` / `%self.api_base`. If an operator sets `CQS_LLM_API_BASE=https://user:secret@vllm.internal/v1` (a documented form for self-hosted vLLM/OpenAI-compatible endpoints), every `LocalProvider::new` and every per-pass startup emits a `tracing::info!` line carrying the secret. With the daemon running under systemd-journald (the OB-V1.30-1 default), the credential lands in journald and is retained per the rotation policy. SEC-V1.36-2 closed the related "every CQS_* env var dumped" path for env vars but left the resolved-config field unguarded. Note also `tracing::debug!(?config, ...)` at `src/config.rs:758` re-logs the loaded config struct via `Debug`, surfacing the same field whenever debug logging is on.
- **Suggested fix:** Add a `redacted_api_base()` method on `LlmConfig` that uses the same scheme-find / `@`-find logic already in `resolve()`, then route every logging site through it. As a smaller patch, mutate `LlmConfig.api_base` to its redacted form *after* the `Client::builder()` build (the Client already has the original credentials embedded in its keep-alive connection); the field then becomes safe to log and `Debug`-print. Add a regression test that calls `LlmConfig::resolve` with a `user:pass@` URL and asserts neither `format!("{:?}", cfg)` nor `format!("{}", cfg.api_base)` contains `pass`.

#### SEC-V1.38-2: `parsed_anthropic_message` debug log can echo prompt content
- **Difficulty:** easy
- **Location:** `src/llm/batch.rs:86-93`
- **Description:** Partial follow-up to SEC-V1.36-2 / P2-#33. The raw HTTP error body is no longer logged, but `parsed.error.message` is logged at debug level as `parsed_anthropic_message`. Anthropic's 400-class responses regularly echo offending input fragments — schema validation errors include the failing JSON path values, and `invalid_request_error` for content policy includes the rejected content snippet. With `RUST_LOG=cqs=debug` (recommended in `CONTRIBUTING.md`), submitting a chunk that contains an embedded API key (real or simulated by the chunk) into the LLM batch can land that key in journald via Anthropic's echo. The `redacted_api_message` path correctly avoids this for the user-visible `LlmError::Api`; the debug log undoes the same protection.
- **Suggested fix:** Drop `parsed_anthropic_message` from the debug log; keep `body_len` + `parsed.error.type` (the structural code, e.g. `"invalid_request_error"`, which doesn't echo input). For local debugging the operator can read the raw response by re-running with `tracing::trace!` (which is gated even tighter than debug).

#### SEC-V1.38-3: `write_proposed_patch` non-atomic write — interrupted run leaves corrupt patch on disk
- **Difficulty:** easy
- **Location:** `src/doc_writer/rewriter.rs:608-617`
- **Description:** `--improve-docs` proposed-patch path uses a bare `std::fs::write(&patch_path, unified.as_bytes())`. A SIGINT / OOM kill / power loss between the `create_dir_all` and the syscall completion leaves a truncated `.patch` file at the final path. The user's review workflow is `git diff` followed by `git apply .cqs/proposed-docs/**/*.patch` (and per #1506, `<ref-dir>/proposed-docs/**/*.patch` for refs). `git apply` on a truncated unified diff produces partial source-file changes — silent corruption of source. The same module already has an `atomic_write` helper at line 634 (used by `rewrite_file`) that does temp-write + rename. The proposed-patch arm bypasses it.
- **Suggested fix:** Route line 617 through the existing `atomic_write` (`fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()>`) or `crate::fs::atomic_replace`. Same one-line change; same crash-safety guarantee `--apply` already gets.

#### SEC-V1.38-4: `cmd_ref_update` skips the SEC-V1.30.1-6 symlink-redirect warning that `cmd_ref_add` carries
- **Difficulty:** medium
- **Location:** `src/cli/commands/infra/reference.rs:631-641` (vs. the original `cmd_ref_add` at `:222-260`)
- **Description:** `cmd_ref_add` (since #1222 / SEC-V1.30.1-6) emits a loud `tracing::warn!` + stderr `WARN:` when `dunce::canonicalize` redirects the user-supplied source path through a symlink. The matching `cmd_ref_update` arm reads `ref_config.source` (the *stored* path from `.cqs.toml`), checks `source.exists()` without canonicalizing, then hands it to `enumerate_files` which canonicalizes inside. If between `cqs ref add` and `cqs ref update` the source path got swapped to a symlink — e.g. an operator pointed `vendored-deps/` at `~/work/customer-A-private/` after the initial add — the update silently re-indexes the new target. No symlink notice fires, so the operator who pulls a poisoned ref from somewhere they didn't expect has no signal. The same window applies to `cmd_ref_reindex` per #1506 (calls into the same `cmd_ref_update` body via the new flag-parity path).
- **Suggested fix:** Lift `symlink_redirect_warning(&source_input, &canonical_source)` out of `cmd_ref_add` into a `pub(crate)` helper and call it from `cmd_ref_update` as well, after `enumerate_files` has canonicalized. The check is cheap (lexical normalization, no FS calls beyond the canonicalize already performed) and surfaces the same threat — vendored content swap — that SEC-V1.30.1-6 was filed for.

#### SEC-V1.38-5: `serve` `chunk_detail` / `static_asset` log user-controlled path strings at info level without size cap
- **Difficulty:** easy
- **Location:** `src/serve/handlers.rs:202`; `src/serve/assets.rs:58`
- **Description:** Both handlers emit `tracing::info!(chunk_id = %id, ...)` / `tracing::info!(path = %path, ...)` against axum-extracted path params. The auth middleware sits inside the route layer (good — these only fire post-auth), but the path values are unbounded user input. An attacker with a valid token (e.g. someone given temporary local access for pair debugging) can inflate journald by sending hundreds of KB-long path strings — each request appends them verbatim to the journal at info level (the daemon's default for OB-V1.30-1). A 100 KB path × 1000 requests = 100 MB of journal bloat per attacker session. Less load-bearing: the SEC-V1.30.1-2 OB-V1.30.1-10 fix demoted `serve::search` query-text logging from info → debug specifically because of journald retention; these two handlers were missed.
- **Suggested fix:** Cap the logged value at ~256 chars: `tracing::info!(chunk_id = %id.chars().take(256).collect::<String>(), ...)`. Or move the user-controlled value to debug level and log only `id_len` at info, mirroring the SEC-V1.30.1-2 pattern in `serve::search`.

#### SEC-V1.38-6: `validate_and_read_file` uses pre-canonicalize path for existence + size checks (TOCTOU window plus redundant existence check)
- **Difficulty:** medium
- **Location:** `src/cli/commands/io/read.rs:31-63`
- **Description:** Sequence: `dunce::canonicalize(&file_path)` (line 42) → `file_path.exists()` (line 47) → `std::fs::metadata(&file_path)` (line 53) → `std::fs::read_to_string(&canonical)` (line 62). Two issues. (1) `canonical` already proved `file_path` exists (`canonicalize` requires it), so line 47's `exists()` on the *unresolved* path is dead — and worse, on an interleaving where a symlink at `file_path` got swapped between line 42 and line 47, `exists()` could re-evaluate a now-broken link as false. (2) line 53 calls `metadata(&file_path)` (the *unresolved* path) for the size cap, but line 62 reads `&canonical`. A symlink swap between line 53 and line 62 — file_path now points to /etc/passwd, canonical still points to the original location — bypasses the size cap on the read. SECURITY.md acknowledges TOCTOU as a generic standard-FS-race, but the inconsistent use of `file_path` vs `canonical` widens the window unnecessarily.
- **Suggested fix:** After line 42, drop line 47 entirely (canonicalize already proved existence) and run line 53 against `&canonical` instead of `&file_path` (so the size check and the read both reference the same resolved inode). Optional belt: open the file once via `File::open(&canonical)` then derive metadata + content from the open handle, eliminating the residual race.

#### SEC-V1.38-7: `run_git_diff`'s base-ref check misses `--upload-pack=…` family of arg-injection refs
- **Difficulty:** easy
- **Location:** `src/cli/commands/mod.rs:574-582`
- **Description:** Validation rejects `b.starts_with('-')` and `b.contains('\0')`. That blocks `--upload-pack=...` because it starts with `-`. But `git diff` accepts `--` as an end-of-options sentinel; without it, refs that *contain* `..` or that match git's range syntax (`HEAD...attacker-controlled`) are valid. More relevant: refs containing `\n` / `\r` aren't blocked (git strips them at parse time, but the validation fails to assert that). The `b.starts_with('-')` check is also insufficient if a future caller ever prepends positional args before `b` — e.g. a refactor that adds `cmd.arg("--no-pager"); cmd.arg(b);` doesn't change behavior, but a refactor that reorders `cmd.arg(b); cmd.arg("--something");` makes `b = "--malicious"` reachable since `cmd.arg` doesn't shift the leading-dash check. The defensive layer is too thin.
- **Suggested fix:** After the existing checks, prepend the `--` sentinel before the user-supplied ref: `cmd.args(["--no-pager", "diff", "--no-color", "--", b])` — actually that doesn't work for `diff` since `--` separates revs from paths there; use `cmd.args(["--no-pager", "diff", "--no-color"]).arg(b)` and add a stricter regex check on `b` (alphanumeric + `._/~^@-` only, length ≤ 255). Also reject `\n`, `\r`, `\t` explicitly to match git's own ref-name validation rules per `git check-ref-format`.

#### SEC-V1.38-8: `tracing::debug!(path = %path.display(), ?config, "Loaded config")` dumps full Config including secrets-bearing fields
- **Difficulty:** easy
- **Location:** `src/config.rs:758`
- **Description:** The `Config` struct includes `llm_api_base: Option<String>` and is serialized via `?config` (Debug). With `RUST_LOG=cqs=debug` set, every `Config::load` call emits the entire struct including `llm_api_base` — and any `https://user:pass@host` userinfo therein. Same redaction gap as SEC-V1.38-1 but at the config-load site rather than the LLM-resolve site. The check applies even to read-only paths (every `cqs search` re-loads config), so the leak surface is every command, not just `cqs index --llm-summaries`.
- **Suggested fix:** Implement a hand-written `Debug` for `Config` that elides `llm_api_base`'s userinfo (delegate to the same redactor as SEC-V1.38-1's helper). Or remove the `?config` from this specific line — `path = %path.display()` is enough to confirm "we loaded a config from here" without spilling the contents.

#### SEC-V1.38-9: `enforce_concurrency_cap` cap defaults from env without validating upper bound
- **Difficulty:** medium
- **Location:** `src/serve/mod.rs:318-320`; `src/limits.rs::serve_max_concurrent_requests` (search by name)
- **Description:** `Semaphore::new(serve_max_concurrent_requests())` sizes the cap from a single env var read at startup. If an operator sets `CQS_SERVE_MAX_CONCURRENT_REQUESTS=4294967295` (or any unrealistically large number) the semaphore allocates a 32-bit counter that effectively disables the cap, defeating SEC-V1.36-9 / #1461. There's no max-bound validation. Combined with `serve --no-auth --bind 0.0.0.0` (an explicit but documented opt-in), an attacker can saturate the daemon with any number of concurrent in-flight requests, each pulling 64 KiB of body buffer + a SQLite read connection from the pool.
- **Suggested fix:** Cap the env override to a sane ceiling at parse time: `min(env_or_default, 1024)`. The semaphore stays correctly bounded regardless of operator-supplied values. Log a `tracing::warn!` when the env override would have exceeded the ceiling so the operator sees the clamp.

#### SEC-V1.38-10: `find_pdf_script` candidate-ownership check + python exec are racy under symlink swap
- **Difficulty:** medium
- **Location:** `src/convert/pdf.rs:88-141`
- **Description:** `validate_script_safety(&candidate)` (line 127) checks `metadata(path)` for uid + mode; the script then gets `Command::new(&python).arg(&script_path)` (line 30 via subsequent `convert_pdf_to_md`) where `script_path` may be the *string* of `candidate.to_string_lossy().to_string()`. Between the metadata check and the python interpreter `open()` of the script, a co-located attacker who can write to any directory in the candidate chain (e.g., `scripts/` if the user runs `cqs convert` from a project with a contributor-supplied `scripts/`) can replace `pdf_to_md.py` with a symlink to their own script. Python opens the symlink target, executes it. SEC-V1.25-8 added the ownership check but didn't address the swap window. Also: line 117-135 iterates candidates including `current_exe()'s ../scripts/pdf_to_md.py`, which on a writable install dir (`~/.cargo/bin/`) is exploitable if anyone else writes to that dir. Documented as a generic FS race in SECURITY.md, but the gap is wider than the doc suggests because the check + exec aren't on the same fd.
- **Suggested fix:** Open the script with `File::open(path)` after canonicalize, fstat the open fd via `metadata().unwrap()` on the file (not the path) — `File::metadata` runs against the inode, not the path, closing the symlink-swap window. Then pass `/proc/self/fd/<n>` (Linux) or the canonicalized path (other platforms) to python so the same inode is what gets executed. Alternative: ship the script embedded via `include_str!` exactly as `umap.rs` already does (`UMAP_SCRIPT`), eliminating both the on-disk script and the entire ownership-check problem. The PDF script is small enough to embed.

---

## Summary

10 findings against current `main` (post-#1505/#1506). Themes:

- **Userinfo leakage in API base logs** (SEC-V1.38-1, SEC-V1.38-8): `LlmConfig::api_base` is logged raw at five sites and the full `Config` is `?config`-printed at debug; no redactor on the field itself. Fix: redact at the type level (`Debug` impl + helper getter) so all sites are protected.
- **Prompt-content echo via Anthropic error messages** (SEC-V1.38-2): SEC-V1.36-2's `body=%body` removal didn't cover the parsed `error.message` field, which Anthropic populates with input fragments. Drop it.
- **Patch-write atomicity** (SEC-V1.38-3): #1506's `proposed-docs` patches use bare `fs::write`. Crash mid-write → corrupt diff → silent source-file damage on `git apply`. Reuse the existing `atomic_write` helper in the same file.
- **Symlink-redirect notice missing on update path** (SEC-V1.38-4): SEC-V1.30.1-6 only landed in `cmd_ref_add`. `cmd_ref_update` (and #1506's reindex parity arm) silently re-indexes through swapped symlinks.
- **Path TOCTOU + redundant checks in `cqs read`** (SEC-V1.38-6): inconsistent use of `file_path` vs `canonical` widens the symlink-swap window between size-cap and read.
- **Defensive layers thinning out**: `git diff` ref validation (SEC-V1.38-7) and `cqs serve` concurrency cap (SEC-V1.38-9) both have validation that's barely enough today and breaks under foreseeable refactors / hostile env vars.
- **Subprocess script swap window** (SEC-V1.38-10): SEC-V1.25-8 closed the ownership check but the validate-then-exec gap remains; embed the script (mirror `umap.rs`).
- **Path-param log inflation** (SEC-V1.38-5): two serve handlers missed the OB-V1.30.1-10 demote-to-debug pattern; cap the value or move to debug.

No prompt-injection sandbox regressions found — `sanitize_untrusted` and the per-prompt nonce sentinel still hold (the speculatively-mentioned `build_prompt_with_envelope` from the audit prompt does not exist in the current tree). #1506's `<ref-dir>/proposed-docs/<rel>.patch` construction is structurally safe because chunks are canonicalized at index time, but the non-atomic write (SEC-V1.38-3) is the real defect on that path. #1505's `--model` drift check is sound — it only adds a refuse-to-run guard, no new attack surface.

---

## Data Safety

# Data Safety Audit (post-v1.38.0)

Eight findings. Three concern #1452 (`needs_embedding` flag) wiring gaps in
read paths that pull zero-vec sentinels into derived data structures
(CAGRA / UMAP / neighbor brute-force / base HNSW). Two concern slot-aware
path resolution drift in the batch daemon. The remainder cluster around
HNSW load-vs-save TOCTOU and a backup-prune disk-burn regression.

#### DS-V1.38-1: `embedding_batches` does NOT filter `needs_embedding=0` — CAGRA / UMAP / neighbors load zero-vec sentinels
- **Difficulty:** easy
- **Location:** `src/store/chunks/async_helpers.rs:549-624` (`EmbeddingBatchIterator::next`)
- **Description:** The sibling `EmbeddingHashBatchIterator` (line 660) was patched for #1452 to add `AND needs_embedding = 0`. The non-hash variant — exposed via `Store::embedding_batches()` and `Store::embedding_base_batches()` — was not. Production callers that consume zero-vec sentinels:
  - `src/cagra.rs:780` (CAGRA build) — pushes zero-vecs straight into the cuVS `flat_data` buffer; the resulting CAGRA index advertises a search neighborhood of "things near (0,0,...,0)" for every #1452 chunk. CAGRA is the default backend at chunk_count ≥ 5000 + GPU available.
  - `src/cli/commands/index/umap.rs:86` (UMAP projection) — projects zero-vecs alongside real ones; cluster view collapses #1452 chunks to a single (or pathological) point.
  - `src/cli/commands/search/neighbors.rs:108` (brute-force kNN) — `dot()` against zero-vec returns 0, contaminating the result set with low-relevance hits.

  `embedding_base_batches` is incidentally safe because the v18→v19 upsert path writes `embedding_base = NULL` for #1452 chunks (per the bind at async_helpers.rs:370-374) and the SELECT filters `embedding_base IS NOT NULL`. The plain `embedding` column gets the zero-vec sentinel by design (see batch_insert_chunks line 353), so any caller of `embedding_batches` is exposed.

  The foreground enriched HNSW build is safe because `build_hnsw_index_owned` uses `embedding_and_hash_batches` (the patched iterator). The CAGRA build is the same shape but uses the unpatched iterator.
- **Suggested fix:** In `EmbeddingBatchIterator::next` (line 565), append `AND needs_embedding = 0` to the SQL string. The base column path already filters `embedding_base IS NOT NULL`, so a single concatenation handles both column variants. Add a unit test that inserts a chunk via `upsert_chunks_unembedded_batch` and asserts `embedding_batches` does NOT yield it.

#### DS-V1.38-2: `enrichment_pass` clears `needs_embedding=0` but never repopulates `embedding_base` — base HNSW permanently misses #1452 chunks
- **Difficulty:** medium
- **Location:** `src/store/chunks/crud.rs:468-477` (`update_embeddings_with_hashes_batch`'s `UPDATE chunks SET ...`) and `src/store/chunks/async_helpers.rs:425` (ON CONFLICT clause)
- **Description:** Two compounding behaviors leave `embedding_base` permanently NULL for any chunk that ever passed through `--llm-summaries` first-pass-skip:
  1. `upsert_chunks_unembedded_batch` writes `embedding_base = NULL` on initial insert (correct per the comment at async_helpers.rs:363-369).
  2. `update_embeddings_with_hashes_batch` (the enrichment-pass embedding writer) updates `embedding`, `enrichment_hash`, and `needs_embedding=0` — but NOT `embedding_base`. So after enrichment, the chunk has a real `embedding`, `needs_embedding=0`, and a permanent `embedding_base = NULL`.
  3. The ON CONFLICT clause in `batch_insert_chunks` (line 425) overwrites `embedding_base = excluded.embedding_base` on EVERY conflict where `content_hash` or `parser_version` changed. So a content-changed re-upsert via `upsert_chunks_unembedded_batch` overwrites a previously-good `embedding_base` with NULL, and the same enrichment gap means it never recovers.

  Net effect: every `--llm-summaries` reindex permanently degrades the base HNSW (`build_hnsw_base_index` filters `WHERE embedding_base IS NOT NULL`), which is the routing target for DenseBase strategy (conceptual / behavioral / negation queries — exactly the queries where enriched embeddings hurt). The `--llm-summaries` performance win (#1452, ~halves GPU time) silently trades search quality on those query classes.
- **Suggested fix:** In `update_embeddings_with_hashes_batch`, when the row's prior state was `needs_embedding=1` AND `embedding_base IS NULL`, also write `embedding_base = t.embedding`. (Same source bytes as `embedding` because enrichment_pass first writes the raw NL embedding; the post-enrichment overwrite of `embedding` only happens later on the second pass.) Or — restructure the pipeline so the very first enrichment write fills `embedding_base` before the call-context enriched embedding lands in `embedding`. Add a regression test: insert via `upsert_chunks_unembedded_batch`, run enrichment, assert `base_embedding_count() == 1`.

#### DS-V1.38-3: `BatchContext::check_index_staleness` watches the LEGACY index path, not the active slot — daemon caches go stale silently
- **Difficulty:** easy
- **Location:** `src/cli/batch/mod.rs:483, 607, 788, 2043, 2337` (every `cqs_dir.join(INDEX_DB_FILENAME)` site)
- **Description:** `BatchContext::cqs_dir` is set in `create_context_with_runtime` to `cqs::resolve_index_dir(&root)`, which returns the project `.cqs/` directory (line 2317). Then `check_index_staleness` (line 483) and `from_path` (line 2337) both watch `cqs_dir.join(INDEX_DB_FILENAME)` — i.e. `.cqs/index.db`. After PR #1105 (per-slot directories), the actual store opens at `.cqs/slots/<name>/index.db`. The staleness check now targets a path that doesn't exist on slot-migrated projects: `DbFileIdentity::from_path` returns `None`, the warn at line 491 fires once, and the daemon's mutable caches (HNSW, SPLADE, call_graph, file_set, notes, refs) NEVER invalidate when the operator runs `cqs index`. Operator workflow: edit code → `cqs index` → daemon keeps serving stale results.
  
  The five sites at lines 483, 607, 788, 2043, 2337 all share the same path-construction pattern. The Store opens correctly via `resolve_index_db` (line 2318) which DOES honor slot resolution; only the staleness-tracking `cqs_dir.join` sites drifted.
- **Suggested fix:** Resolve the active slot once at `BatchContext` construction and store the slot dir alongside (or in place of) `cqs_dir`. Replace each `cqs_dir.join(cqs::INDEX_DB_FILENAME)` with the slot-aware path. Add an integration test that creates `.cqs/slots/default/index.db`, opens a `BatchContext`, touches the slot DB, and asserts the next staleness check observes the change.

#### DS-V1.38-4: HNSW `load_with_dim` existence check happens BEFORE the shared lock — concurrent saver can rename files out from under reader
- **Difficulty:** medium
- **Location:** `src/hnsw/persist.rs:706-739` (existence check at 706, lock acquisition at 734-758)
- **Description:** The reader sequence is:
  1. Check `graph_path.exists() && data_path.exists() && id_map_path.exists()` (line 706). If any missing → return `NotFound`.
  2. Open + `try_lock_shared` on `<basename>.hnsw.lock` (line 734-752).
  3. Call `verify_hnsw_checksums` and proceed.

  Between steps 1 and 2, a concurrent `save()` can take the exclusive lock and rename graph/data/ids files into `.bak`. The reader then waits up to ~1s for the shared lock; if save finishes within that window, the reader proceeds with files that have already been replaced — `verify_hnsw_checksums` reads the NEW bytes vs an OLD checksum file (or vice versa, depending on rename order) and fails. The user sees a confusing checksum-mismatch error during a normal concurrent rebuild instead of either a clean retry or a clean read of the new index.

  The deeper hazard: the HNSW save renames extensions one at a time inside `rename_result` (lines 527-559). There is no atomic "all four files swap together" primitive on POSIX. A reader that grabs the shared lock between the `graph` rename and the `data` rename observes a graph from save N+1 paired with data from save N — a corrupt index that may pass checksum verification if the per-file checksum file was also half-renamed in the same window.
- **Suggested fix:** Move the file-existence check INSIDE the shared-lock critical section (re-check after `try_lock_shared` succeeds). For the half-renamed concern: the saver should write the NEW checksum file LAST so a reader observing a mismatched set fails the checksum check rather than loading corrupt data. Even better — bundle all four files into a single `<basename>.hnsw.bundle` and atomically replace the bundle, eliminating the half-state entirely. (Larger refactor; the existence-check-under-lock fix is the easy mitigation.)

#### DS-V1.38-5: Migration backup `prune_old_backups` swallows `read_dir` errors and emits `tracing::error!` only — no operator-actionable signal
- **Difficulty:** easy
- **Location:** `src/store/backup.rs:243-258` (post-DS-V1.36-10 partial fix)
- **Description:** DS-V1.36-10 was triaged as a P3 fix. The current implementation upgrades the failure log from `warn!` to `error!` with an `approx_dir_bytes` field, but still returns `Ok(())` to the caller. The migration's caller at `src/store/migrations.rs:122-126` already classifies prune failure as "non-fatal — the user's DB is at the correct version", which means a transient permission glitch that also happens during every subsequent migration produces:
  1. New backup written successfully (`copy_triplet`).
  2. `read_dir` for prune fails again → warn-then-Ok.
  3. Loop repeats indefinitely; `.bak-v*-v*-*.db` accumulates one new backup per migration with zero KEEP_BACKUPS enforcement.

  The `approx_dir_bytes` log addition helps post-mortem but doesn't solve the unbounded growth. On a heavily-migrating CI rotating through schema versions, this is hundreds of MB per run.
- **Suggested fix:** Track a per-process counter of consecutive prune failures; on the second failure, switch from "Ok with log" to surfacing the error so the migration caller can downgrade to a more visible warn (or emit a metric). Alternative: enforce KEEP_BACKUPS as a HARD CAP — if the prune step can't even read the dir, then before the NEXT migration, refuse to write a new backup unless the operator clears the dir manually. The "fail-loud-if-unbounded-growth" stance matches the spirit of CQS_MIGRATE_REQUIRE_BACKUP=1.

#### DS-V1.38-6: `WRITE_LOCK` is process-global, not per-Store — multi-store callers serialize all writes globally
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:54` (`static WRITE_LOCK: Mutex<()> = Mutex::new(());`)
- **Description:** The `static` Mutex serializes writes across every `Store` instance in the process. SQLite is one-writer-per-database, so within a single `Store` this is correct. But multiple `Store` instances against DIFFERENT databases (e.g. ReferenceIndex-cached refs in the daemon's `refs` LRU; eval scripts opening multiple slot DBs in parallel; future multi-tenant daemon serving slot A and slot B) all contend on the same global mutex. The daemon's reference indexes (`Arc<Mutex<lru::LruCache<String, Arc<ReferenceIndex>>>>` at batch/mod.rs:291) are read-only on the dispatch path so this isn't exploited today, but a future write-side feature (e.g. background ref reindex) would block unrelated slot-A writes on slot-B's pending writer.

  Additionally: `MutexGuard<'static, ()>` is held across `pool.begin().await` (line 1206-1207). If the await-suspends-the-task pattern lands in tokio (current sqlx behavior is non-suspend, but contract is async) this could deadlock the daemon. The DS-5 comment at line 1 acknowledges the cross-await hold.
- **Suggested fix:** Replace the `static` with a `Mutex<()>` field on `Store`. Each Store instance gets its own write lock, which is the correct granularity. Cross-process writer exclusion is already provided by SQLite's file locking. Run the existing concurrent-writer test suite to ensure the per-Store lock is sufficient (it should be — every existing test opens one Store).

#### DS-V1.38-7: `write_active_slot` uses `crate::temp_suffix()` for tmp-file naming but does NOT hold the `slots.lock` — read-modify-update of active_slot races with concurrent `slot promote`
- **Difficulty:** easy
- **Location:** `src/slot/mod.rs:647-694` (`write_active_slot`) — caller contract issue, not the function itself
- **Description:** `write_active_slot` validates the name and atomically replaces `.cqs/active_slot`. The DS-V1.33-2 fix added `temp_suffix()` so concurrent writers don't collide on a fixed `active_slot.tmp` name. But the function itself takes no lock — it relies on every CALLER holding `acquire_slots_lock` first. Production callers do (`slot_promote` at slot.rs:292, `slot_create` at slot.rs:239). However, three caller sites bypass the lock:
  - `migrate_legacy_index_to_default_slot` at slot/mod.rs:997 — runs INSIDE the slots lock (line 844), correct.
  - Test callers at slot/mod.rs:1248, 1296, 1308, 1320 — fine, single-threaded.
  - **Future risk**: any new caller that forgets the lock (e.g. a `cqs slot set-default` shortcut) silently loses updates. The function's doc comment doesn't state the lock requirement.
- **Suggested fix:** Either (a) acquire `slots.lock` inside `write_active_slot` itself (idempotent — slots.lock is reentrant via OS-level flock), making the function self-contained, or (b) add a `#[must_use]` / explicit `&FlockGuard` parameter that forces the caller to prove they hold the lock. Update the function doc to state the lock requirement either way.

#### DS-V1.38-8: v26→v27 migration adds `needs_embedding` column — but enrichment_pass on a freshly-migrated DB sees zero `needs_embedding=1` rows, so nothing forces a base-embedding repopulation
- **Difficulty:** medium
- **Location:** `src/store/migrations.rs:984-1010` (migrate_v26_to_v27) + interaction with `enrichment_pass`
- **Description:** The v26→v27 migration adds the column with `DEFAULT 0`. Pre-existing rows are stamped `needs_embedding=0`, treating them as already embedded. That's correct from a content-bytes standpoint — they DO have a real embedding. But the migration silently inherits one bug from the prior schema: any pre-v18 row that never went through Phase 5 has `embedding_base = NULL` (per migrate_v17_to_v18 at line 622-627). The v27 migration doesn't touch `embedding_base`, so post-migration these rows remain invisible to `build_hnsw_base_index`. The user sees no log indicating their base-HNSW coverage is partial.

  Compounding with DS-V1.38-2: a v27-migrated user who then runs `cqs index --llm-summaries` on changed content will write `embedding_base = NULL` for those changed chunks (per ON CONFLICT clause), and enrichment never repopulates. So the base index coverage erodes monotonically over time on `--llm-summaries` workflows.
- **Suggested fix:** Add an explicit operator-visible signal: after migration to v27, log the count of `embedding_base IS NULL AND needs_embedding = 0` rows at `info!` level. Once DS-V1.38-2 is fixed, queue these for re-embedding via the enrichment_pass `needs_embedding=1` mechanism — trigger the repopulation by a one-shot `UPDATE chunks SET needs_embedding=1 WHERE embedding_base IS NULL` in the v27 migration so the next index pass actually fills them. (This is the non-trivial fix; the log-only mitigation is the easy variant.)

---

## Performance

# Performance Audit (post-v1.38.0)

Scope: hot-path allocations, N+1 patterns, missing caches, env-var thrash, redundant I/O. Focus on CLI/daemon search and indexing critical paths. Audit mode ON (no notes consulted).

Prior PF-V1.36-* findings already addressed; this batch covers gaps and post-v1.36 regressions.

---

#### PF-V1.38-1: `is_test_chunk` rebuilds 54-language pattern lists per call from inside the per-candidate scoring loop
- **Difficulty:** medium
- **Location:** `src/lib.rs:498-533` (`is_test_chunk`) → `src/language/mod.rs:977-1040` (`all_test_path_patterns`, `all_test_name_patterns`)
- **Description:** `apply_scoring_pipeline` (search/scoring/candidate.rs:368) calls `chunk_importance` → `is_test_chunk(name, file_path)` once per candidate. Each call iterates `language::REGISTRY.all_test_name_patterns()` and `all_test_path_patterns()`, both of which **rebuild a deduplicated `Vec<&'static str>` plus a `HashSet<&str>` from scratch by walking all 54 language definitions** (`for def in self.all() { for pat in def.test_path_patterns { seen.insert(*pat); ... } }`). Inside `is_test_chunk` itself: `let normalized = file.replace('\\', "/")` allocates a String even on Linux where no replacement happens, plus per-pattern `sql_like_matches` allocates another String (`pattern.replace("\\_", "_")`) and a `Vec<&str>` from `split('%').collect()`. For 500 candidates × ~60 path patterns + ~12 name patterns the per-query cost is roughly 30k–60k allocations. Hits every search at default `enable_demotion = true`. Bigger repos with more languages amplify — and `TEST_NAME_PATTERNS` rebuild has cost roughly proportional to language count.
- **Suggested fix:** Wrap both `all_test_*` calls in `OnceLock<Vec<&'static str>>` on `LanguageRegistry` (the registry itself is static — patterns can never change at runtime). Skip the `replace('\\', '/')` when `!file.contains('\\')` (cheap byte-scan). Hoist `sql_like_matches`'s `Vec<&str>` parts to a per-pattern `OnceLock`.

#### PF-V1.38-2: SPLADE `id_map: Vec<String>` not migrated to `Vec<Box<str>>`
- **Difficulty:** easy
- **Location:** `src/splade/index.rs:165, 578, 768`
- **Description:** PR #1502 migrated HNSW and CAGRA `id_map` from `Vec<String>` to `Vec<Box<str>>` (saves 8 bytes per entry: 24 → 16). SPLADE was not migrated despite identical access pattern (build-once, read-many; mutated only by `push` during `build`). For an 18k-chunk index at 32-char chunk ids, missing ~144 KB of heap savings per slot with a SPLADE backend. Stacks with HNSW/CAGRA savings on multi-slot setups.
- **Suggested fix:** Apply the #1502 pattern: change `id_map: Vec<String>` to `Vec<Box<str>>`, push `chunk_id.into_boxed_str()` in `build`, deserialize via `String::into_boxed_str()` in load path (line 578 area). The two read sites (`get(idx)` + `clone()` on line 257) work unchanged because `Box<str>` derefs to `&str`.

#### PF-V1.38-3: `resolve_splade_alpha` allocates `format!` and reads two env vars per search query
- **Difficulty:** easy
- **Location:** `src/search/router.rs:730-796`
- **Description:** Called once per search via `dispatch_search`. On every call: `format!("CQS_SPLADE_ALPHA_{}", category.to_string().to_uppercase())` allocates a String (line 743), then `std::env::var(&cat_key)` (744) and `std::env::var("CQS_SPLADE_ALPHA")` (781) syscall the env table. Comment on line 743 even acknowledges "hot path" yet no caching. The rest of the function then takes a `RwLock::read()` for the slot table. For batch-mode handlers that fire many searches per session, the env reads dominate the hot path post-fusion.
- **Suggested fix:** `OnceLock<HashMap<QueryCategory, Option<f32>>>` for the per-cat env-derived value (keyed by enum variant, so no string formatting). Same for the global `CQS_SPLADE_ALPHA` — `OnceLock<Option<f32>>`. The slot/preset/default fall-through stays as-is. Test `test_type_boost_factor_reads_env_on_each_call` indicates eval sweeps mutate env between searches in a single process — gate caching behind a "first read wins" model documented in the knob, or add a test-only reset hook (mirrors `reset_classifier_vocab_for_test`).

#### PF-V1.38-4: SPLADE `splade_max_chars()` and `default_threshold()` re-read env on every encode call
- **Difficulty:** easy
- **Location:** `src/splade/mod.rs:387-393` (`splade_max_chars`), `397+` (`default_threshold`), `815-819` (`CQS_SPLADE_MAX_SEQ` in `encode_batch`)
- **Description:** `encode()` is called per-chunk during indexing and per-query during search. For an 18k-chunk full reindex with SPLADE, that's 18k env-var lookups for `CQS_SPLADE_MAX_CHARS` plus 18k for `CQS_SPLADE_THRESHOLD`. `encode_batch` parses `CQS_SPLADE_MAX_SEQ` once per batch (less hot, but same shape). All three values are immutable for the process lifetime in production.
- **Suggested fix:** `OnceLock<usize>` / `OnceLock<f32>` initializers, mirroring the `PLACEHOLDER_CACHE`/`MULTISTEP_PATTERNS_AC` pattern already established. Document via `// cached at process start` comment so eval harnesses know not to mutate mid-run.

#### PF-V1.38-5: Tree-sitter `Parser::new()` allocated per `parse_file` call
- **Difficulty:** medium
- **Location:** `src/parser/mod.rs:295, 554, 914`; `src/parser/aspx.rs:234, 342, 478`; `src/parser/calls.rs:50, 314, 687`; `src/parser/injection.rs:275`; `src/parser/l5x.rs:76`
- **Description:** Every call to `parse_file` / `parse_file_all_with_chunk_calls` constructs a fresh `tree_sitter::Parser` and calls `set_language(&grammar)`. For a fresh index over 600+ files that's 600+ parser allocations + grammar reloads. Tree-sitter parsers are reusable across files of the same language — `set_language` is the only language-specific call. Recursion-depth state lives on each `parse()` invocation. The Parser struct already caches Queries lazily in `OnceCell` but parser instances aren't cached.
- **Suggested fix:** Add `parsers: HashMap<Language, Mutex<tree_sitter::Parser>>` to the Parser struct (alongside `queries`). Each `parse_file` does `let mut p = self.parsers.get(&lang).unwrap_or_init().lock(); p.set_language(...)?; let tree = p.parse(...);`. For multi-threaded indexing use `thread_local!` pools. Aspx/L5X subparsers benefit identically.

#### PF-V1.38-6: `gather::expand` clones `Arc<str>` neighbor to `String` for HashMap key on every BFS expansion
- **Difficulty:** easy
- **Location:** `src/gather.rs:362-364`
- **Description:** Inside the BFS expansion loop: `visited.insert(Arc::clone(&neighbor)); let key: String = neighbor.to_string(); name_scores.insert(key, ...);` — the neighbor is already an `Arc<str>` (cheap to clone), but `to_string()` materializes a fresh owned String. For a gather BFS that expands ~1000 nodes (default `max_expanded_nodes`) in a dense graph that's 1000 redundant String allocations.
- **Suggested fix:** Change `name_scores: HashMap<String, (f32, usize)>` to `HashMap<Arc<str>, (f32, usize)>`; use `Arc::clone(&neighbor)` (atomic incr, no alloc). The downstream `name_scores.get(name.as_ref())` (line 347) and `fetch_and_assemble` (line 397) consumers work unchanged via `Borrow<str>`.

#### PF-V1.38-7: `extract_call_snippet_from_cache` allocates full `Vec<&str>` of all chunk lines just to pick 3
- **Difficulty:** easy
- **Location:** `src/impact/analysis.rs:183-189`
- **Description:** `let lines: Vec<&str> = best.chunk.content.lines().collect();` allocates a Vec sized to the entire chunk's line count, then indexes `lines[start..end]` to extract a 3-line window. For chunks 100+ lines long and N callers per impact target (build_caller_info loop at line 140), this is `N × Vec::with_capacity(line_count)` of wasted allocation. The chunk content is already in memory; only the 3-line slice is needed.
- **Suggested fix:** Replace with `let snippet: Vec<&str> = best.chunk.content.lines().skip(start).take(end - start).collect(); Some(snippet.join("\n"))`. The iterator is lazy — only the 3 lines we keep get visited. Saves both the intermediate Vec and the work of fully iterating long chunks.

#### PF-V1.38-8: Watch reindex stats `abs_path` twice per file (mtime + size) — duplicate syscall
- **Difficulty:** easy
- **Location:** `src/cli/watch/reindex.rs:649-718`
- **Description:** First stat at line 659 (`abs_path.metadata().and_then(|m| m.modified())`) inside the `mtime_cache.entry(file.clone()).or_insert_with(...)` block. Second stat at line 718 (`std::fs::metadata(&abs_path).ok().map(|m| m.len())`) for the v23 fingerprint write-back. Both are on the same file in the same code block (line 717 already does `let abs_path = root.join(file);` again — second `PathBuf::join` allocation too). On WSL 9P the per-stat latency is ms-scale, so a 200-file watch tick burns 200ms on duplicate stats alone.
- **Suggested fix:** Combine into one `match abs_path.metadata() { Ok(m) => (m.modified(), Some(m.len())), Err(_) => (Err(_), None) }` so size + mtime come from one syscall. Hoist `let abs_path = root.join(file);` above the mtime block so the second `join` is gone too.

#### PF-V1.38-9: `analyze_type_impact` makes 4+ string clones per (chunk × type) edge in nested loop
- **Difficulty:** medium
- **Location:** `src/impact/analysis.rs:471-490`
- **Description:** For each `(type_name, chunks)` and each `chunk` in chunks: `shared.entry(chunk.name.clone()).or_default().insert(type_name.clone());` followed by `chunk_info.entry(chunk.name.clone()).or_insert((chunk.file.clone(), ...));`. That's 4 String/PathBuf clones per edge. For a popular type used by 200 functions × M types in scope, that's 800+×M clones — only 1 per unique name actually survives `.entry().or_default()`. The `chunk.name.clone()` happens twice per chunk regardless of whether either entry was new.
- **Suggested fix:** Use `shared.entry_ref(&chunk.name).or_insert_with(|| HashSet::new()).insert(...)` (hashbrown's `entry_ref` borrows the lookup key, allocates only on insert). Or pull `let name = &chunk.name;` and use `match shared.get_mut(name)` first, falling through to `shared.insert(name.clone(), ...)` on miss. Same shape for `chunk_info`. `type_name.clone()` is unavoidable for `HashSet<String>` insert but could be `Arc<str>` if inflation matters.

#### PF-V1.38-10: `find_test_chunks_cross` calls uncached `find_test_chunks()` once per project — N×LIKE-scans
- **Difficulty:** easy
- **Location:** `src/store/calls/cross_project.rs:217-237`
- **Description:** Loops `for ns in &self.stores { ns.store.find_test_chunks() }`. `find_test_chunks` is the per-store LIKE-scan PF-2 already flagged as uncached (`LIKE '%marker%'` over the BLOB content column). Cross-project users with N references pay N × full-table-scan per call. Sibling `merged_call_graph` (line 244) uses `ensure_all_graphs()` — a cache pattern — but `find_test_chunks_cross` doesn't.
- **Suggested fix:** Either (a) add a `RwLock<Option<Arc<Vec<TestChunkSummary>>>>` cache to each `Store` (mirrors `note_boost_cache`) so the per-project scan is amortized across the cross-project caller; or (b) in `CrossProjectStore`, cache the merged result at the cross-project level and invalidate only when an underlying store reports a write. (a) is the more general fix — `find_test_chunks` has 14 callers per PF-2.

---

## Summary

Ten findings, mostly easy, several with measurable per-query cost on the search hot path. Highest-leverage targets:

1. **PF-V1.38-1** (`is_test_chunk` registry rebuild) — fires per candidate per search; quick `OnceLock` patches knock 30k+ allocations off every query. Highest priority by far.
2. **PF-V1.38-3** + **PF-V1.38-4** (env var thrash on every search/encode) — small individually, compound on hot paths. SPLADE encode in particular hits 18k env reads per reindex.
3. **PF-V1.38-5** (tree-sitter Parser per file) — 600× per fresh index; the structural work to add a Mutex pool is real but the win is durable.
4. **PF-V1.38-2** (SPLADE id_map Box<str>) — exact analog of merged #1502; trivial.
5. **PF-V1.38-7** + **PF-V1.38-8** (snippet `lines().collect()` and duplicate stat) — easy hot-path wins on impact + watch reindex.

Findings 6, 9, 10 are medium-impact tidiness with solid call-path leverage. None of these have been triaged in audit-batch1-scaling.md or prior PF-V1.* files.

Worth noting: the search hot path (`search_filtered` → `search_by_candidate_ids_with_notes`) has been heavily optimized through PF-V1.25-* and PF-V1.36-* work — the surface left is mostly in upstream / downstream modules (router, scoring helpers, registry lookups) rather than the inner scoring loop itself.

---

## Resource Management

# Resource Management Audit (post-v1.38.0)

Scope: idle daemon cost, cache TTLs, ONNX session lifecycle, subprocess buffer caps, unbounded buffer growth, cross-slot embedding cache cap enforcement.

Prior context: triage `docs/audit-triage.md` (v1.36.2). RM-V1.36-1/2/4 closed in #1456; #1471 dropped daemon idle CPU to ~0; #1502 trimmed HNSW id_map RSS via `Box<str>`.

## Findings

#### RM-V1.38-1: EmbeddingCache eviction never fires on a query-only daemon
- **Difficulty:** easy
- **Location:** `src/cli/watch/mod.rs:1390-1400` (gated inside the reindex branch); `src/cli/batch/mod.rs:867-873` (one-shot `warm()` evict only)
- **Description:** `evict_embeddings_cache_with_runtime` is wired in three places: `cqs index` pipeline tail, `BatchContext::warm()` once at daemon startup, and the watch reindex branch (1 hr throttle). A daemon serving only queries — no file events for hours/days — does not enter the reindex branch and so never trims `embeddings_cache.db` after the boot-time call. `CQS_CACHE_MAX_SIZE` (default 10 GB) and `CQS_QUERY_CACHE_MAX_SIZE` (100 MB) become advisory until the next file-change burst. Worst case: a small project queried heavily (eval runs, multi-agent batch) without code edits accumulates blob writes via the search path's caching of new embeddings without ever hitting the cap-enforcement path.
- **Suggested fix:** add an idle-tick gated cache evict alongside `sweep_idle_sessions` in `daemon.rs:130` (the existing 60-s minute tick) — open the cache, call `evict()`, drop, on a 1-hr cadence regardless of file-event activity. Mirror the existing throttle counter from `mod.rs:1213` (`last_cache_evict`) but located in the daemon-thread loop.

#### RM-V1.38-2: LocalProvider stash cap is per-batch-count, not per-byte
- **Difficulty:** easy
- **Location:** `src/llm/local.rs:73, 427-449`
- **Description:** `MAX_STASH_BATCHES = 128` evicts old batches when the count exceeds 128, but a single batch holds `HashMap<custom_id, response_text>`. With LLM summary passes producing 5–10 KB responses for tens of thousands of items per batch, one un-fetched batch can be 100s of MB; 128 of them is multi-GB. The eviction key is `keys().min()` (lex-smallest UUID) — random eviction order means a slow consumer can't predict survival of its own batch.
- **Suggested fix:** add a byte budget alongside the count cap (e.g. `MAX_STASH_BYTES = 256 * 1024 * 1024`). Track running total during insert, evict by oldest-insert (use `IndexMap` for FIFO order, not lex UUID) until under both budgets. Lex-min eviction is documented as "evictee is unfetched — that's a leak signal" but the order is still arbitrary; FIFO is no harder.

#### RM-V1.38-3: PLACEHOLDER_CACHE backing Vec is permanently sized to 32,467 OnceLocks
- **Difficulty:** medium
- **Location:** `src/store/helpers/sql.rs:76-84`
- **Description:** `PLACEHOLDER_CACHE: Vec<OnceLock<String>>` is allocated at length 32,467 (≈ 520 KB metadata) on first use and lives for process lifetime. Each `OnceLock<String>` populated by a query stays populated forever — there is no eviction. A daemon that hits varied batch sizes (e.g., 47, 234, 1,000, 8,116) over its lifetime accumulates one full placeholder string per distinct n; the largest single string is ~190 KB at n = 32,466 (`?32466` × 32466). Common batch sizes total ~1–2 MB; a daemon that touches every distinct n on a busy reindex burst tops ~3 GB worst case. The doc-comment dismisses memory cost as "microseconds to allocate" but only counts the metadata, not populated cells.
- **Suggested fix:** populate strings only for n the daemon actually uses (already true — lazy), but cap the Vec length at a smaller bound (e.g. `MAX_CACHED_PLACEHOLDER_N = 4096`, covers all observed prod usage) and fall back to `build_placeholders` for larger n. Past 4 K, the per-call build cost is microseconds and the saved memory is real.

#### RM-V1.38-4: UMAP subprocess `wait_with_output` buffers full stdout/stderr
- **Difficulty:** easy
- **Location:** `src/cli/commands/index/umap.rs:171-180`
- **Description:** `child.wait_with_output()` buffers the entire stdout (which carries `n_rows` × ~64-byte coordinate lines, ~64 MB at 1 M chunks) and unbounded stderr in RAM before yielding `output.stdout`. A python script wedged in a tight error-print loop — or a UMAP run on a multi-million-chunk corpus — will OOM the indexer process. Sibling fix RM-V1.36-2 hardened `pdf_to_markdown`; this site shipped after that PR landed.
- **Suggested fix:** stream stdout line-by-line via `BufReader::lines().take(MAX_LINES)` per the v1.36.2 RM-V1.36-7 pattern (with per-line cap). Cap stderr capture at 64 KB (truncate-with-marker) — operators only need the tail for diagnostics.

#### RM-V1.38-5: `export_model.rs` ONNX export uses unbounded `Command::output()`
- **Difficulty:** easy
- **Location:** `src/cli/commands/train/export_model.rs:60-62, 74-86`
- **Description:** Two `Command::output()` calls (deps probe + actual `optimum.exporters.onnx` invocation) buffer subprocess stdout/stderr unbounded. The second call invokes Python optimum, which on a large model export prints multi-MB progress logs; on a wedged HuggingFace download it can hang for hours while RAM grows. Same pattern flagged in RM-V1.36-5 (chm/convert/train_data) but `export_model` was missed in the sweep.
- **Suggested fix:** spawn + `take(MAX_BYTES)` on both stdout and stderr handles with `wait_timeout` (e.g. 30 min for the export, 30 s for the deps probe). Apply the same `RM-V1.36-5` fix shape.

#### RM-V1.38-6: BatchContext SPLADE encoder slot held forever even after data-cache eviction
- **Difficulty:** medium
- **Location:** `src/cli/batch/mod.rs:294 (field), 403-456 (sweep_idle_sessions)`
- **Description:** `sweep_idle_sessions` clears the SPLADE *session* (line 427-433) after `CQS_BATCH_IDLE_MINUTES`, and the data caches (HNSW, splade_index, call_graph, test_chunks, file_set, notes_cache) after `CQS_BATCH_DATA_IDLE_MINUTES`. But the `splade_encoder: Arc<OnceLock<Option<SpladeEncoder>>>` itself is never reset to `None`; only its inner ONNX session is freed. The `SpladeEncoder` struct still holds the tokenizer (~10 MB BPE), the decoded vocab map, and pinned model paths. On a long-idle daemon this is small but real, and the asymmetry with the embedder/reranker (which share the same `clear_session` contract) is surprising. More importantly: a slot/model swap that changes the SPLADE config can't take effect because `OnceLock` has no `take()` mid-flight.
- **Suggested fix:** wrap the slot in `Mutex<Option<...>>` (mirroring `splade_index`) and have the data-cache eviction branch null it out. Already paired with the data caches (28 mins of idle = no SPLADE work coming) so dropping the encoder costs nothing on the steady-state path.

#### RM-V1.38-7: `last_indexed_mtime` prune tied to size threshold, not age — long-idle daemon never trims
- **Difficulty:** easy
- **Location:** `src/cli/watch/gc.rs:60-70`; `src/cli/watch/events.rs:258`
- **Description:** `prune_last_indexed_mtime` early-returns when `map.len() <= 5_000` (default). A daemon that has indexed 4,999 files and then sits idle for weeks holds those 4,999 entries forever — `process_file_changes` is the only caller and only fires on file events. On a small project this is fine; on a slowly-growing project that crosses the threshold during a burst then quiesces, the prune fires once and the trigger never re-arms. The age cutoff (`LAST_INDEXED_PRUNE_AGE_SECS`) only matters when the size gate opens.
- **Suggested fix:** add a periodic age-based prune call from the watch loop's idle branch (already runs `cycles_since_clear` book-keeping at `mod.rs:1413`), gated to once per hour. Reuses the same cutoff logic; makes the trim bounds time-shaped rather than peak-shaped.

#### RM-V1.38-8: SOCKET-thread tokio runtime worker count not adaptive — fixed at startup
- **Difficulty:** medium
- **Location:** `src/cli/watch/runtime.rs:56-77`
- **Description:** `build_shared_runtime` resolves `worker_threads = min(num_cpus, 4)` at process start and never adjusts. On idle, all 4 workers stay parked but pin their stack (default 2 MB each = 8 MB). On a 16-core workstation under heavy load, `CQS_DAEMON_WORKER_THREADS` is the only escape — but no per-load adjustment. Tokio doesn't shrink multi-thread runtimes; this is a tokio-design constraint. Worth noting because the doc says "shrink workers on idle" is an audit cut to make.
- **Suggested fix:** keep multi_thread for hot-path concurrency; document explicitly in `runtime.rs` that workers are static for the daemon's life and any operator wanting different sizing must restart with `CQS_DAEMON_WORKER_THREADS`. Optionally: set `thread_stack_size(512 * 1024)` to bound the parked-worker RSS contribution to 2 MB instead of 8 MB.

#### RM-V1.38-9: Notes cache (`Arc<Vec<Note>>`) has no size accounting; eviction tied only to mtime/idle
- **Difficulty:** medium
- **Location:** `src/cli/batch/mod.rs:282 (notes_cache field)`; `src/store/metadata.rs:338-361 (cached_notes_summaries)`
- **Description:** The daemon's `notes_cache` field caches the full `Vec<Note>` for the project. Notes are typically small (KB), but a project running heavy `cqs notes add` ingestion (e.g. importing a large external observation set) can grow this past 10s of MB. There is no max-size cap; eviction depends on the data-cache idle timer (default 30 min) or an index-mtime change. Symmetric for `Store::notes_summaries_cache` (RwLock-backed inside Store) — both grow with note count without an upper bound.
- **Suggested fix:** add a soft cap on `notes_cache` (e.g. 5,000 notes or 32 MB serialized), beyond which the cache returns from the DB on each call instead of caching. For the typical project (200–500 notes) the cap never trips; for runaway note-churn cases the daemon stops accumulating.

#### RM-V1.38-10: `LocalProvider` `pool_max_idle_per_host = concurrency` survives forever via `OnceLock` reuse
- **Difficulty:** easy
- **Location:** `src/llm/local.rs:157-162`
- **Description:** The `reqwest::Client` is built with `pool_max_idle_per_host(concurrency)` and `pool_idle_timeout(30s)` — sound. But the `LocalProvider` itself is constructed once per CLI invocation; in the daemon path, it's held inside the `LlmClient` for the daemon's lifetime. The 30-s idle pool prune happens, but each client thread that hits `submit_via_chat_completions` over time accumulates *crossbeam_channel* sender/receiver pairs (line 221). They're cleaned via `std::thread::scope`'s join, so per-batch — but if `submit` is interleaved across many small batches against many distinct local LLM hosts (e.g. multi-server eval comparing vLLM endpoints), each new endpoint string spawns a fresh in-memory pool with no cross-batch reuse. Minor — flagged for the audit cut about "connection lifecycle."
- **Suggested fix:** confirm only one `LocalProvider` exists per `LlmClient` per host (looks correct from current callers but no unit test). Add a `Drop` impl that explicitly clears `self.stash` and logs un-drained batches at warn — an operator hitting the `MAX_STASH_BATCHES` warning has no easy way to spot the leak without it.

## Summary

Ten findings, mostly easy. The dominant theme: **TTL/eviction logic is well-built but tied to file events** rather than wall-clock time. A daemon that serves queries 24/7 without code changes drifts past every cap (`embeddings_cache.db`, `query_cache.db`, `last_indexed_mtime`, SPLADE encoder slot, notes_cache) — the eviction infrastructure exists but the periodic-tick wiring is incomplete on the read-only path. RM-V1.38-1 is the highest impact (cache eviction never fires), then RM-V1.38-4/-5 (subprocess output buffering, sibling regressions to RM-V1.36-2/-5). RM-V1.38-3 (PLACEHOLDER_CACHE 32k slots) is medium-impact but documented as "by design" — the cap is the right shape, not the OnceLock-per-n approach. RM-V1.38-2 closes a per-batch-bytes gap in the prior "128 batch count cap" fix. The remaining items (RM-V1.38-6/-7/-9/-10) are smaller asymmetries worth tightening but not single-handedly load-bearing.

The pattern to extract for the v1.38.x triage: **every cap that depends on a file-event tick needs a parallel idle-tick path** — the daemon's poll-driven loop (#1471) made the file-event-tick assumption stale.

---

## Test Coverage (happy path)

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

---
