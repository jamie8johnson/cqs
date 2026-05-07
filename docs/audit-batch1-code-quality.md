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

