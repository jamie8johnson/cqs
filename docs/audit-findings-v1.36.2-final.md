# Audit Findings (v1.36.2)

Auditing v1.36.2 (post-#1455 release, post-v1.33.0 audit completed previously).

Branches: main @ `ad921040` (clean, in sync with origin).

Open tracking issues from prior audits (do NOT re-file):
- #1095 — P4 umbrella (2 micro-perf items)
- #1096 — SEC-7 serve auth
- #1097 — EX-V1.29-1 Commands trait
- #1107 — slot create --model not persisted
- #1108 — content_hash storm in 5 hot SELECTs

Findings are appended below by category.


<!-- ===== docs/audit-scratch/batch1-code-quality.md ===== -->
## Code Quality

#### CQ-V1.36-1: Enrichment hot path drops `model_max_seq_len`, falls back to 512 default
- **Difficulty:** medium
- **Location:** src/nl/mod.rs:43-59 (and src/cli/enrichment.rs:223)
- **Description:** Configurable-models disaster pattern, *new* instance not in v1.33 triage. `enrichment_pass` (production reindex hot path called from `cli/commands/index/build.rs:634`) takes `model_config: &ModelConfig` as a parameter (line 26) but threads only `summary`/`hyde` into `cqs::generate_nl_with_call_context_and_summary` (line 223). That function then calls the legacy 1-arg `generate_nl_description(chunk)` (src/nl/mod.rs:59) which routes through `generate_nl_with_template` → `CQS_MAX_SEQ_LENGTH` env (default 512). Result: for nomic-coderank (2048-tok) or any model with seq > 512, every enriched chunk's section-content preview is capped at ~1800 chars instead of the model-correct ~8000. The initial embedding pipeline (`cli/pipeline/embedding.rs:146`, `cli/watch/reindex.rs:520`) already calls `generate_nl_description_with_seq_len(c, model_max_seq_len)`, so enrichment silently undoes part of that work. P1-28 (#1330) was claimed as "fixed" in v1.33 triage — the fix only updated the initial embedding sites; the enrichment pass was missed.
- **Suggested fix:** Add `model_max_seq_len: usize` parameter to `generate_nl_with_call_context_and_summary`, plumb through to `generate_nl_with_template_and_seq_len(chunk, NlTemplate::Compact, model_max_seq_len)` instead of the legacy 1-arg call. Pass `model_config.max_seq_length()` from `enrichment_pass`. Verify with `grep -rn "generate_nl_description\b" src/` returning only test sites and the convenience-wrapper definition itself.

#### CQ-V1.36-2: `resolve_splade_model_dir()` zero-arg wrapper drops `[splade]` config at all 6 production sites
- **Difficulty:** easy
- **Location:** src/splade/mod.rs:213-215
- **Description:** Configurable-models disaster pattern: `pub fn resolve_splade_model_dir() -> Option<PathBuf>` calls `resolve_splade_model_dir_with_config(None)` — passing `None` is documented as "match the legacy no-config behavior". Six production callers use the zero-arg variant: `cli/store.rs:321`, `cli/batch/mod.rs:941` and `:1804`, `cli/commands/index/build.rs:689`, `cli/watch/events.rs:286`, `cli/watch/reindex.rs:69`. Every one ignores the user's `.cqs.toml` `[splade]` section (`preset`, `model_path`, `tokenizer_path`). Users editing config see no effect on watch/index/batch paths; only env vars work. Doc comment at `:208` warns "this single helper is the *only* place SPLADE paths are resolved" — but the helper itself silently strips config.
- **Suggested fix:** Either delete the zero-arg wrapper and force every call site to thread the loaded `cfg.splade.as_ref()`, or change the wrapper to accept a `Config`/`&AuxModelSection` and convert all call sites. The latter is mechanical; the former exposes the wiring gap at the type level. Add an integration test that sets `[splade] preset = "..."` in `.cqs.toml` and asserts watch/reindex picks it up.

#### CQ-V1.36-3: `generate_nl_with_template` legacy env-only entry point still public, masks bug from CQ-V1.36-1
- **Difficulty:** easy
- **Location:** src/nl/mod.rs:197-203
- **Description:** `generate_nl_with_template(chunk, template)` reads `CQS_MAX_SEQ_LENGTH` (default 512) and forwards to `_with_template_and_seq_len`. It is `pub`, re-exported from `lib.rs:232`, and is the path through which `generate_nl_description` (1-arg) and the enrichment hot path silently truncate previews. Doc comment admits: "legacy 1-arg API kept for compatibility". With CQ-V1.36-1 fixed, this wrapper has zero non-test callers. Leaving it `pub` invites the same disaster (any new caller picks the env-default rather than the model-correct value).
- **Suggested fix:** After CQ-V1.36-1 is fixed, downgrade `generate_nl_with_template` to `pub(crate)` (or delete entirely if the test at `:815` is updated to call `_with_template_and_seq_len`). Same for `generate_nl_description`. Both wrappers are tombstones for an API that should require the seq-len parameter.

#### CQ-V1.36-4: Stale dead-code reports in `cqs dead` for src/llm/* — index out of sync with v1.33 trait unification
- **Difficulty:** easy
- **Location:** Tooling artifact; affects audit/triage workflow.
- **Description:** `cqs dead --json` flags `submit_doc_batch`/`submit_hyde_batch` at `src/llm/batch.rs:387,395`, `src/llm/local.rs:681,689`, `src/llm/provider.rs:153,161,207,215` as dead. Those names no longer exist (PR #1347 / EX-V1.33-1 unified them into `submit_batch`); only doc-comments and historical eval JSON mention them. The HNSW index has stale chunk_id mappings — `cqs read` at those line numbers shows the new code. This isn't a code defect but it actively wastes auditor time chasing ghosts. `assert_eq!(human_bytes(...))` and `format_uptime(...)` are similarly mis-flagged because the `_with_seq_len` macro/test reflection isn't traced.
- **Suggested fix:** Run `cqs index` (or `systemctl --user restart cqs-watch`) to refresh post-#1347. Longer term: dead-code analyzer should require the chunk's content to be reachable from a live caller-graph node, not just match a chunk-id from the snapshot when the file at that line no longer contains the function. Equivalently, attach the indexed-at content_hash to dead entries and skip when the on-disk hash differs.

#### CQ-V1.36-5: 5 public NL-gen entry points where 2 would suffice (wrapper proliferation)
- **Difficulty:** easy
- **Location:** src/nl/mod.rs:43,167,175,197,209
- **Description:** Public surface: `generate_nl_with_call_context_and_summary` (7 args), `generate_nl_description` (1 arg), `generate_nl_description_with_seq_len` (2 args), `generate_nl_with_template` (2 args, env-default), `generate_nl_with_template_and_seq_len` (3 args). Three of these are 1-line forwarders to a more-parameterized variant. Only `_with_call_context_and_summary` and `_with_template_and_seq_len` carry semantic content. The 1-arg `generate_nl_description` is the trap from CQ-V1.36-1. This is mostly dead surface area — `lib.rs:232` re-exports both `generate_nl_with_call_context_and_summary` AND `generate_nl_with_template`, and the 1-arg `generate_nl_description` is exported via the unqualified glob from older releases.
- **Suggested fix:** After CQ-V1.36-1, retain only `generate_nl_with_template_and_seq_len` (free-form) and `generate_nl_with_call_context_and_summary` (enrichment). Make the latter take `model_max_seq_len`. Mark the three legacy entry points `#[deprecated]` for one release, then delete (per project policy: no external users, no deprecation cycle needed). The `tests/eval_test.rs:57` use of `generate_nl_description` should switch to the seq-len variant so eval mirrors production.

#### CQ-V1.36-6: `enrichment_pass` accepts `model_config` but ignores it
- **Difficulty:** easy
- **Location:** src/cli/enrichment.rs:23-28, :223-231
- **Description:** Function signature takes `model_config: &cqs::embedder::ModelConfig` (line 26) but the parameter is read only by surrounding logic in `enrichment.rs` not visible in the relevant block — when calling `generate_nl_with_call_context_and_summary` at line 223, only `summary` and `hyde` are threaded, not the model. The compiler can't catch this because `model_config` is used elsewhere in the function (e.g., for batch sizing). This is the classic "parameter exists but isn't propagated to the place it should constrain" bug — same shape as the configurable-models disaster.
- **Suggested fix:** Once CQ-V1.36-1 widens the NL signature, pass `model_config.max_seq_length()` (or whichever accessor is correct — verify against `embedder/models.rs`) through. Add a regression test: enrich a chunk with both 512-seq and 2048-seq models, assert the produced NL string differs in length.

#### CQ-V1.36-7: `VectorIndex::search_with_filter` default impl uses unchecked `k * 3`
- **Difficulty:** easy
- **Location:** src/index.rs:103
- **Description:** Trait default impl for filter-aware search: `self.search(query, k * 3).into_iter().filter(...)`. P1-42 (claimed fixed in #1326) addressed `limit*3`/`limit*2` overflow in the brute-force scoring path under `src/search/`, but this trait default — used by any backend that doesn't override (e.g., the brute-force `Vec<Embedding>` backend) — still has the same shape. With `k = usize::MAX/2` (legitimate-looking large `k` from a misconfigured `--limit` env), `k * 3` overflows in release without panic. A test harness or mis-routed daemon request could trip this.
- **Suggested fix:** Replace with `k.saturating_mul(3)`. Same one-token fix as P1-42; likely just missed because triage scoped to `src/search/`.


<!-- ===== docs/audit-scratch/batch1-documentation.md ===== -->
## Documentation

#### lib.rs Features list omits qwen3-embedding-{4b,8b} presets
- **Difficulty:** easy
- **Location:** src/lib.rs:9
- **Description:** The `## Features` list in the crate-top docstring enumerates configurable embedding presets as "embeddinggemma-300m default since v1.35.0; bge-large, bge-large-ft, E5-base, v9-200k, nomic-coderank, and custom ONNX presets". `src/embedder/models.rs` registers two more first-class presets — `qwen3-embedding-8b` (line 521, #1392 ceiling probe) and `qwen3-embedding-4b` (line 579, shipped in v1.36.1 PRs #1441 + #1442). Missing them in the most-public docstring (the rustdoc landing) is a lying-docs P1: a developer reading the lib root won't discover the two largest-context presets the crate actually exposes.
- **Suggested fix:** Append `qwen3-embedding-8b, qwen3-embedding-4b` to the preset enumeration on line 9 (and mirror the same enumeration in `README.md:672` and `README.md:780` — see follow-up finding).

#### README "How It Works" + CQS_EMBEDDING_MODEL row omit qwen3 presets
- **Difficulty:** easy
- **Location:** README.md:672, README.md:780
- **Description:** `README.md:672` (How It Works → Embed) lists "embeddinggemma-300m default since v1.35.0; bge-large, bge-large-ft, E5-base, v9-200k, nomic-coderank presets". `README.md:780` (CQS_EMBEDDING_MODEL row) advertises the accepted values as `embeddinggemma-300m, bge-large, bge-large-ft, v9-200k, e5-base, nomic-coderank`. Both miss `qwen3-embedding-8b` and `qwen3-embedding-4b`, which are accepted by `ModelConfig::from_preset` and ship with built-in pin tests (`models.rs:1078 qwen3_embedding_4b_preset_shape`). CONTRIBUTING.md has the same gap at lines 237 / 239 / 353. PRIVACY.md "Model Download" (lines 30-36) likewise stops at `nomic-coderank` and omits the qwen3 family despite both repos being downloaded by users who pick them.
- **Suggested fix:** Update README.md:672, README.md:780, CONTRIBUTING.md:237/239/353, and PRIVACY.md "Model Download" bullets to include both qwen3 presets, with a short note on context length (qwen3 is 4096-cap per the v1.36.1 cap change in CHANGELOG).

#### Cargo.toml `lang-all` feature missing `lang-elm` (54-vs-53 mismatch)
- **Difficulty:** easy
- **Location:** Cargo.toml:244
- **Description:** The `lang-all` umbrella feature lists 53 entries: `lang-rust … lang-st, lang-dart`. `lang-elm` is intentionally a registered language (`Elm` variant in `src/language/mod.rs`, definition shipped, `lang-elm` is in `default = […]` on line 187), but `lang-all` does **not** include it. A user running `cargo build --no-default-features --features lang-all` will get a 53-language build that silently drops Elm. README/Cargo.toml otherwise advertise "54 languages" everywhere.
- **Suggested fix:** Insert `"lang-elm"` into the `lang-all` array on line 244 (between `lang-elixir` and `lang-erlang` to match registry order).

#### `src/language/mod.rs` crate-doc Feature Flags list missing `lang-dart`
- **Difficulty:** easy
- **Location:** src/language/mod.rs:10-65
- **Description:** The `# Feature Flags` enumeration in the module-top docstring lists 53 `lang-*` features (lang-rust through lang-aspx, lang-st, then `lang-all`). It is missing `lang-dart`, which is registered at `src/language/mod.rs:1094` (`Dart => "dart", feature = "lang-dart"`) and shipped as a default feature in `Cargo.toml:243/187`. Currently 54 lang features in the registry; docstring lists 53.
- **Suggested fix:** Add `//! - \`lang-dart\` - Dart support (enabled by default)` immediately above the `lang-all` line at src/language/mod.rs:65.

#### CHANGELOG.md `[Unreleased]` section is at wrong position (between 1.34.0 and 1.33.0)
- **Difficulty:** easy
- **Location:** CHANGELOG.md:194
- **Description:** Per Keep-a-Changelog (which the file's own header points to), `[Unreleased]` lives at the top above the latest released version. cqs's CHANGELOG has versioning order `[1.36.2] (line 8) → [1.36.1] (25) → [1.35.0] (116) → [1.34.0] (144) → [Unreleased] (194) → [1.33.0] (196) → …`. `[Unreleased]` is empty and orphaned between two released versions. Either it was forgotten when 1.34.0 was cut, or it was meant to be deleted. Tools that parse the changelog (release notes generators, dependabot) will see an empty `[Unreleased]` ordered below released versions and can produce broken release notes.
- **Suggested fix:** Either delete the empty `[Unreleased]` block at line 194-195, or move it above `## [1.36.2] - 2026-05-04` on line 8. Given the project ships fast and unreleased changes accumulate, moving it to the top is the right answer.

#### Cargo.toml description over-rounds eval R@20 (89% claimed, 88.6% measured)
- **Difficulty:** easy
- **Location:** Cargo.toml:6
- **Description:** Crate description at line 6 reads: "51% R@1 / 76% R@5 / 89% R@20 on v3.v2 dual-judge code-search (218 queries, EmbeddingGemma-300m default …)". README's matching TL;DR on line 5 cites "50.9% R@1 / 76.2% R@5 / 88.6% R@20" — same eval, same fixture, same date. 88.6% rounds to 89% only by typical rounding; 50.9 rounds to 51 cleanly and 76.2 rounds to 76. The 89% value will be cited verbatim by crates.io users who don't read the README, and overstates the result by 0.4pp. This is exactly the lying-docs cluster (docs that promise behavior the code/eval doesn't deliver) that team policy flags as P1.
- **Suggested fix:** Either round all three consistently (51% / 76% / 89% → keep, but note in description "≈"), or use the README's precise numbers (51% / 76% / 89% → 50.9% / 76.2% / 88.6%). Recommend matching the README exactly: "50.9% R@1 / 76.2% R@5 / 88.6% R@20".

#### SECURITY.md cites stale `src/lib.rs:601` for `enumerate_files` follow_links
- **Difficulty:** easy
- **Location:** SECURITY.md:223
- **Description:** SECURITY.md "Symlink Behavior → Directory walks" promises: "`enumerate_files` (`src/lib.rs:601`) sets `WalkBuilder::follow_links(false)`". The function `enumerate_files` is now at `src/lib.rs:757` (eager wrapper) and `enumerate_files_iter` at `src/lib.rs:786`; the actual `.follow_links(false)` call lives at `src/lib.rs:813`. A reader auditing the security claim by jumping to line 601 lands in the middle of an unrelated section and is left wondering whether the claim is still true. P1 lying-docs (the behavior is correct, the citation is wrong).
- **Suggested fix:** Update the citation to `src/lib.rs:813` (the `follow_links(false)` line) — that's the load-bearing line. Or to `src/lib.rs:786` if pointing at the function rather than the line. Both `enumerate_files` and `enumerate_files_iter` exist now, so the "function name" citation is also slightly stale.

#### `src/schema.sql` header still says "schema v22" — actual is v26
- **Difficulty:** easy
- **Location:** src/schema.sql:1
- **Description:** First line of `src/schema.sql` reads `-- cq index schema v22`. The actual current schema is v26 (`CURRENT_SCHEMA_VERSION: i32 = 26` in `src/store/helpers/mod.rs:140`); migrations v23, v24 (#1221 vendored), v25 (#1133 notes.kind), and v26 (#1409 composite chunks index) have all landed since the comment was written. Subsequent comments inside the file correctly call out v24/v25/v26 columns (e.g. `chunks.vendored` at line 60 is annotated "v24", `notes.kind` at line 144 is annotated "v25"), so the file is internally inconsistent. A reader trusting the header thinks four migrations don't exist.
- **Suggested fix:** Change line 1 to `-- cq index schema v26 (see src/store/helpers/mod.rs::CURRENT_SCHEMA_VERSION; v22+v23+v24+v25+v26 columns annotated inline below)`.

#### CONTRIBUTING.md schema citation stale at v25 (actual v26)
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:212, CONTRIBUTING.md:226
- **Description:** Line 212 says "store/ - SQLite storage layer (Schema v25, WAL mode)"; line 226 lists migrations as "v10-v25, including … v25 notes.kind". v1.36.0 (#1409) bumped to v26 with the composite `(source_type, origin)` index on `chunks` — CHANGELOG.md:79 documents this. ROADMAP.md:5 and CHANGELOG correctly say v26; CONTRIBUTING.md missed the bump.
- **Suggested fix:** Change "Schema v25" → "Schema v26" on line 212; on line 226 extend the migration list to "v10-v26, including … v25 notes.kind, v26 composite (source_type, origin) index".

#### ROADMAP.md "Current" header points at v1.36.0 — actual current is v1.36.2
- **Difficulty:** easy
- **Location:** ROADMAP.md:3
- **Description:** Line 3 reads `## Current: v1.36.0 (cut 2026-05-03)`. v1.36.1 (#1446, qwen3-4b preset + FP16) and v1.36.2 (#1455, Store::drop checkpoint TRUNCATE→PASSIVE + busy_timeout bump) have both shipped since (CHANGELOG.md:8, line 25). The roadmap "Current" section is the authoritative right-now-state pointer; readers landing here believe the project is two patch releases behind reality. CHANGELOG.md is correct; ROADMAP.md drifted.
- **Suggested fix:** Update the header to `## Current: v1.36.2 (cut 2026-05-04)` and either fold the v1.36.1/v1.36.2 highlights into the Current section or add brief "**v1.36.1 (2026-05-04):**" / "**v1.36.2 (2026-05-04):**" lines analogous to the existing "**v1.35.0 (released 2026-05-02):**" / "**v1.34.0 (2026-05-02):**" pattern at lines 11-13.

#### SECURITY.md telemetry path uses glob `telemetry*.jsonl` — code writes only `telemetry.jsonl`
- **Difficulty:** easy
- **Location:** SECURITY.md:135
- **Description:** SECURITY.md write-access table line 135 lists `.cqs/telemetry*.jsonl` — the asterisk implies rotation or a numbered family. Actual implementation in `src/cli/telemetry.rs:82` writes only the literal `telemetry.jsonl` (no rotation, no `.jsonl.1`, no date stamps), and the module-top docstring on lines 3, 8, 16, 54 consistently uses the singular form. PRIVACY.md:7 is also singular and correct. The glob is a stale wildcard from when rotation was considered but not implemented — minor lying-docs (overstates the file surface a `rm` would need to wipe).
- **Suggested fix:** Change `.cqs/telemetry*.jsonl` to `.cqs/telemetry.jsonl` on SECURITY.md:135 to match what the code actually writes.


<!-- ===== docs/audit-scratch/batch1-api-design.md ===== -->
## API Design

#### `cqs project search` and top-level `cqs <q>` are two semantic search entry points with diverging surfaces
- **Difficulty:** medium
- **Location:** `cqs project search` (src/cli/commands/infra/project.rs:88) vs top-level `Cli` (src/cli/definitions.rs:153)
- **Description:** The top-level `cqs <q>` command exposes ~20 search knobs (`--rrf`, `--rerank`, `--reranker`, `--name-boost`, `--include-type`, `--exclude-type`, `--path`, `--pattern`, `--name-only`, `--no-content`, `-C/--context`, `--expand-parent`, `--ref`, `--include-refs`, `--tokens`, `--no-stale-check`, `--no-demote`, `--splade`, `--splade-alpha`, `--include-docs`, …). `cqs project search` exposes only `-n`, `-t`, `--json`. So the same conceptual operation (semantic search) has wildly different ergonomics depending on which scope you target. Agents who learn the top-level flag set get nothing transferable when they want cross-project search; the cross-project path silently can't filter by language, type, path, or rerank.
- **Suggested fix:** Either (a) flatten the same `SearchArgs` into both commands so the flag surface is identical, or (b) document explicitly that `project search` is the minimal-surface entry-point and add the top 4–5 most-used filters (`-l`, `--include-type`, `--path`, `--rerank`).

#### `cqs project` and `cqs ref` are two registries of external indexes with overlapping responsibilities
- **Difficulty:** medium
- **Location:** `cqs project add/list/remove/search` vs `cqs ref add/list/remove/update`
- **Description:** Both subcommand trees register external code indexes. `ref` adds a "reference index" (with `--weight`, `update` re-indexes from source); `project` adds an existing `.cqs/index.db` to a "cross-project search registry". Top-level search has both `--ref <name>` and `--cross-project` flags that point at these two different registries. Naming overlap is severe: a "project" is a "reference" and vice-versa from the agent's POV. Compare with the documented P3-29 fix (`project register` → `project add`) which only addressed the verb mismatch — the deeper concept duplication remains. Top-level `cqs --help` shows both `ref` and `project` adjacent with no hint of when to use which.
- **Suggested fix:** Pick one noun. Merge the registries: a single `cqs ref add <name> <path-or-source> [--weight N] [--no-index]` covers "external indexed codebase" whether built locally or already-indexed. Drop `project` (or alias). Long-term this also collapses the `--ref`/`--cross-project` flag confusion in search.

#### `cqs index` and `cqs ref update` both build/refresh an index but use opposite verbs and flag sets
- **Difficulty:** easy
- **Location:** `cqs index` (definitions.rs:388) vs `cqs ref update` (cli/commands/infra/reference.rs)
- **Description:** Re-indexing the project is `cqs index --force` (with `--no-ignore`, `--accept-shared-notes`, `--llm-summaries`, `--improve-docs`, `--apply`, `--max-docs`, `--hyde-queries`, `--dry-run`). Re-indexing a registered ref is `cqs ref update <name>` (no flags). Same operation, two verbs (`index` vs `update`), and the ref path skips every quality-affecting flag the project path supports — so a refreshed reference can't get LLM summaries or HyDE queries even though the project index can.
- **Suggested fix:** Either alias `cqs ref update` → `cqs ref reindex` and pass the same `IndexArgs`, or document clearly that ref refresh is intentionally minimal (it isn't — agents will hit this).

#### `--depth` short flag `-d` exists on `impact`/`onboard`/`test-map` but not on `gather` or `trace`
- **Difficulty:** easy
- **Location:** src/cli/args.rs:284 (`gather`), src/cli/commands/graph/trace.rs:32 (`trace`), vs args.rs:310/473/500
- **Description:** API-V1.29-10 added `-d` for parity, but only on three of the five depth-bearing commands. `gather` declares `#[arg(long, default_value_t = DEFAULT_DEPTH_BLAST, visible_alias = "expand")]` with no short. `trace` uses `--max-depth` with `--depth` only as an alias and no short. `cqs gather "x" -d 2` errors with `unexpected argument '-d' found`, while `cqs impact x -d 2` works. The cross-command muscle memory the audit comment promised isn't actually delivered.
- **Suggested fix:** Add `short = 'd'` to `GatherArgs::depth` and to the `trace` `--max-depth` flag (with the existing `--depth` alias).

#### `--rerank` (bool) and `--reranker` (enum) are both live on `cqs <q>` — two flags for one knob
- **Difficulty:** medium (API design, easy code)
- **Location:** src/cli/definitions.rs:215 (`pub rerank: bool`) and 224 (`pub reranker: Option<RerankerMode>`)
- **Description:** Tracked as P2-14 / #1372 but still both shipped. `--rerank` is "boolean shorthand for `--reranker onnx`". The collision is then resolved by `Cli::rerank_mode()` picking enum-over-bool. This is exactly the "two ways to express the same thing" anti-pattern the audit elsewhere flags as cause of agent confusion. Public docstring even names the loser ("Takes precedence over the legacy `--rerank` bool when both are passed"). Per CLAUDE.md "No External Users" this can be a hard deletion.
- **Suggested fix:** Drop `--rerank` (the bool). Documentation says it's "muscle memory" but a hard rename without alias is the project policy (see `--commits` rename in `blame`). #1372 should ship as a deletion, not as the dual-flag state we're in now.

#### `cqs gather --direction` defaults to `both`; `cqs onboard` only walks callees with no direction knob
- **Difficulty:** easy
- **Location:** `cqs gather` (args.rs:286), `cqs onboard` (args.rs:494)
- **Description:** `gather` exposes `--direction both|callers|callees`, but the conceptually parallel `onboard` (also call-chain BFS expansion) silently hardcodes "callees" with no flag. An agent learning "depth + direction" on `gather` cannot transfer to `onboard`. Similarly, `cqs callers <name>` and `cqs callees <name>` are sibling commands but `cqs onboard` doesn't allow specifying the direction at all.
- **Suggested fix:** Add `--direction` to `OnboardArgs` (default `callees` for back-compat) so depth+direction is a uniform pair across `gather`, `onboard`, and `test-map`.

#### `cqs eval --reranker` accepts `none|onnx|llm` but `llm` errors at runtime — flag advertises a non-existent capability
- **Difficulty:** easy
- **Location:** src/cli/commands/eval/mod.rs (RerankerMode) — help text describes `llm` as "reserved for #1220 and currently bails with a 'not yet implemented' error"
- **Description:** Surfacing a placeholder enum variant in `--help` and the value parser is the same scaffold-as-API anti-pattern that got `LlmReranker` demoted in P1-33. The variant exists in the public CLI surface specifically so "production wiring can land without a breaking CLI change", but per "No External Users" / no-deprecation-cycles policy, that argument doesn't apply here. Agents reading `--help` see `llm` as a real choice.
- **Suggested fix:** Drop `Llm` from `RerankerMode` until #1220 actually wires it. Add it back with the implementation; flipping the variant is one-line at the wire-up site.

#### `cqs slot create --model <preset>` exists but `cqs index --model <preset>` doesn't — model preset is split across two commands inconsistently
- **Difficulty:** medium
- **Location:** `cqs slot create --model` (definitions.rs Slot subcommand), `cqs index` (no `--model`), top-level `Cli::model` (definitions.rs:292)
- **Description:** Model selection is on (a) top-level `cqs <q> --model <name>` (search-time), (b) `cqs slot create --model <name>` (slot bootstrap), but NOT on `cqs index --model <name>` (re-index time). To switch models you must `cqs slot create --model X && cqs slot promote X && cqs index`. Model swap inside the active slot also requires going through `cqs model swap`, a third entry point. Three commands manage one concept and the user has to know the right verb for the right context.
- **Suggested fix:** Add `cqs index --model <preset>` that reindexes into the active slot with the new model (or refuses if the recorded model differs, with a hint to use `model swap`). Consolidate around `model swap` as the canonical "change my model" entry point and delete `slot create --model`'s duplicate behaviour, or document the layering explicitly.

#### `IndexBackend::try_open` returns `Option<Box<dyn VectorIndex>>` but error semantics vs `Ok(None)` are unclear
- **Difficulty:** medium
- **Location:** src/index.rs:160 (`pub trait IndexBackend`)
- **Description:** Trait splits "not applicable, try next backend" (Ok(None)) from "store-level abort" (Err). But `IndexBackendError` only carries `Store` / `ChecksumMismatch` / `LoadFailed` — checksum/load failures are described in the doc comment as "self-handled with `tracing::warn!` + `Ok(None)`", meaning two of the three error variants are documented as never-emitted. So implementations have to internalise a "warn-and-return-Ok(None)" convention with no compile-time enforcement, while still importing the error type. Future backend authors will reasonably reach for `LoadFailed` and break the selector contract silently.
- **Suggested fix:** Either (a) drop `ChecksumMismatch`/`LoadFailed` and tighten the trait return to `Result<Option<...>, StoreError>`, or (b) add a `selector_action: enum { Skip, Abort }` to the error so the contract is encoded in types instead of comments.

#### `BatchProvider::set_on_item_complete` requires `&mut self` but other methods take `&self` — forces `Mutex` everywhere
- **Difficulty:** easy
- **Location:** src/llm/provider.rs:150
- **Description:** Trait method signature: `fn set_on_item_complete(&mut self, _cb: OnItemCallback) {}`. All four other methods take `&self`. Callers that hold a `&dyn BatchProvider` (most of the orchestration code) need to either rebuild the provider with the callback baked in or wrap it in a `Mutex<dyn BatchProvider>` just to call this one configuration setter. The comment justifies "callback may be invoked from multiple worker threads concurrently" — which is fine, but that's about callback invocation, not callback registration.
- **Suggested fix:** Builder pattern: take the callback at construction (`AnthropicBatchProvider::new(...).with_on_item_complete(cb)`), drop the trait method entirely. Keeps the provider `&self`-only and the trait shape uniform.

#### Wildcard `pub use` re-exports in `lib.rs` were collapsed for cross_project but `cross_project` is a special-case `pub mod` defined inside `lib.rs`
- **Difficulty:** easy
- **Location:** src/lib.rs:212-220
- **Description:** Per #1372/P3-52 the file replaced wildcard re-exports with explicit lists for `diff`/`gather`/`impact`/`scout`/`task`/`onboard`/`related`. But `cross_project` was special-cased as an inline `pub mod cross_project { pub use crate::impact::cross_project::...; pub use crate::store::calls::cross_project::...; }` instead of being an explicit re-export at the same level as the others. The single-file fix that the audit comment claimed ("each module now lists exactly what crosses the lib boundary") is undermined by this one ad-hoc nested module that pulls from two different crate paths and obscures where the types actually live.
- **Suggested fix:** Either (a) move `cross_project` types into a real `crate::cross_project` module that re-exports the originals, or (b) hoist the explicit `pub use` list to the top alongside the others (so all of `lib.rs`'s re-exports follow one pattern) and document why a virtual module is the right shape for these specific types.

#### Inconsistent `--limit` defaults: 3 (`where`), 5 (most), 10 (`gather`/`neighbors` actually 5/10), 20 (`eval`)
- **Difficulty:** easy (docs) / medium (semantics)
- **Location:** src/cli/args.rs:120/137/291/333/374/491/532/543/559, eval/mod.rs:52
- **Description:** `where` defaults to 3 file suggestions; `gather` to 10 chunks; everything else to 5; `eval` to 20. These are all "max results returned" but agents have to memorize five different defaults. Compare to the `-d` short flag and the `--commits` rename — recent work has been actively normalizing common knobs across commands. `--limit` (the most-used flag in the entire CLI) has no such effort.
- **Suggested fix:** Either (a) standardize all to 5, with the per-command rationale documented inline, or (b) at minimum harmonise gather/where to the dominant 5, and document `eval`'s 20 as an R@K-specific cap with a comment so it doesn't read as random.


<!-- ===== docs/audit-scratch/batch1-error-handling.md ===== -->
## Error Handling

#### EH-V1.36-1: `Embedder::warm` discards embed_query result via `let _ = ... ?;`
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:1113
- **Description:** `let _ = self.embed_query("warmup")?;` propagates the error (good) but the actual `Vec<f32>` result is silently dropped after running the full forward pass. The intent is "warm caches" — fine — but the `let _ =` here is functionally equivalent to `let _: Result<Vec<f32>, _> = self.embed_query("warmup");` if `?` were ever removed in a refactor. More importantly, the Ok-result is meaningful: a returned vector with all-zeros indicates a misconfigured ONNX session that the warm path would not catch. Today `warm()` says "warmed" even if the model produces nonsense.
- **Suggested fix:** Drop the `let _ =` (the function already returns `Result<(), _>`, so just call `self.embed_query("warmup")?;`) and assert the returned vector has length == declared `embedding_dim()` before logging "embedder warmed". Today a misconfigured model that returns shape `[1, 0]` would warm successfully and then fail at first user query.

#### EH-V1.36-2: `train_data` corpus parse path collapses `Err(_)` and panic into one silent skip
- **Difficulty:** easy
- **Location:** src/train_data/mod.rs:419
- **Description:** `Ok(Err(_)) | Err(_) => { corpus_parse_failures += 1; continue; }` lumps together (a) parser returning a real `ParserError` (e.g., grammar load failed, file too large) and (b) a panic caught via `catch_unwind`. Both increment the same counter with no `tracing::warn!` distinguishing which it was — operator can't tell whether the corpus has 5,000 panicking files (a real bug to file) or 5,000 files exceeding the size cap (expected). Compare with the per-commit branch at line 250 which already separates `Ok(Err(e))` (logs `Parse failed`) from `Err(_)` (logs `Parse panicked`).
- **Suggested fix:** Split into two arms identical to the commit-replay branch:
  ```rust
  Ok(Err(e)) => { tracing::debug!(path = %path.display(), error = %e, "Parse failed"); ... }
  Err(_)     => { tracing::warn!(path = %path.display(), "Parse panicked — skipping"); ... }
  ```

#### EH-V1.36-3: `doc_writer` cross-device fallback silently drops backup-restore I/O error
- **Difficulty:** easy
- **Location:** src/doc_writer/rewriter.rs:629
- **Description:** When `fs::write(path, data)` fails inside the cross-device fallback, the code attempts `let _ = std::fs::rename(&backup_path, path);` to restore the original. If that rename ALSO fails (e.g., `path` now half-written and locked, or backup got chmod'd), the user has lost the original file AND received only the original write error in the tracing warn. The backup_path file is left on disk for them to find, but they have no way to know that — the warn at line 631 only mentions `rename_error` and `write_error`, not "backup is at X, restore failed because Y, recover manually."
- **Suggested fix:** Capture the restore result and include backup_path + restore_err in the warn message: `let restore_err = std::fs::rename(&backup_path, path).err();` then in the warn add `restore_failed = restore_err.as_ref().map(|e| e.to_string()), backup_remaining_at = if restore_err.is_some() { Some(backup_path.display().to_string()) } else { None }`. Operator gets actionable recovery info.

#### EH-V1.36-4: `Embedder::pad_id` silently swallows tokenizer pad-id miss with model_config fallback
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:691-694
- **Description:** `tokenizer.get_padding().map(|p| p.pad_id as i64).unwrap_or(self.model_config.pad_id)`. If the tokenizer.json was loaded WITHOUT a `[padding]` section (which can happen with some HF exports), we silently use the model_config default. For BGE-large that's 0, but for any custom model with mismatched pad_id this produces wrong attention masks — embeddings that subtly drift versus the golden output. No warn, no metric.
- **Suggested fix:** Emit `tracing::warn!(model = %self.model_config.name, fallback_pad_id = self.model_config.pad_id, "tokenizer.json has no padding section — using model_config.pad_id")` exactly once via a OnceLock guard. Operators can correlate "embeddings look weird after model swap" with the warn.

#### EH-V1.36-5: `embedder/mod.rs` checksum-verified marker write swallows fs error
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:1528
- **Description:** `let _ = std::fs::write(&marker, &expected_marker);` after a successful `verify_checksum`. If the write fails (disk full, perms), every subsequent process re-runs the blake3 verify of the model.onnx file (~600 MB) on every cold start. Operator never sees why their daemon took 4s to come up — the warn at line 1106 just says "warmed" without distinguishing first-verify cost from post-marker cost.
- **Suggested fix:** `if let Err(e) = std::fs::write(&marker, &expected_marker) { tracing::warn!(path = %marker.display(), error = %e, "Failed to write checksum-verified marker — model will be re-verified next session"); }`

#### EH-V1.36-6: `Store::stored_model_name()` swallows query errors as `None`, masking schema corruption
- **Difficulty:** medium
- **Location:** src/store/metadata.rs:153-161
- **Description:** This function is `pub` and called from `cmd_doctor`, `slot promote`, and three other call sites that branch on "is this a fresh DB or a model-mismatched one." A query error (e.g., metadata table corrupted, sqlite I/O error) gets logged at warn but returns `None`, which every caller interprets as "fresh DB, no model recorded — treat as new". So a corrupted index is silently treated as a fresh one and a brand-new index gets initialized over it on the next `cqs index` call, *destroying the old data*.
- **Suggested fix:** Change return type to `Result<Option<String>, StoreError>` and let callers decide. The current behavior makes every caller default to the "destroy-and-recreate" path when the DB is unreadable — exact opposite of safe.

#### EH-V1.36-7: `slot/mod.rs:725` sentinel detail read silently empties on I/O error
- **Difficulty:** easy
- **Location:** src/slot/mod.rs:715-727
- **Description:** When a previous migration left a sentinel file, the code reports its contents in the error message via `.unwrap_or_default()`. If the sentinel exists but is unreadable (perms, locked by editor, etc.), the operator sees `"Sentinel contents:\n"` (empty) instead of an explanation that the file existed but couldn't be read. They're now stuck — they don't know if the migration fully or partially failed, and the recovery instruction `rm <path>` may delete useful diagnostic info.
- **Suggested fix:** Replace the `.unwrap_or_default()` chain with explicit Ok/Err handling: on Err, set `detail = format!("(could not read sentinel: {})", err)`. Operator now sees the I/O error and can `chmod`/recover before deleting.

#### EH-V1.36-8: `where_to_add::compute` silently skips files missing from batch-fetched chunks
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:215-217
- **Description:** `all_origins_chunks.remove(origin_key.as_ref()).unwrap_or_default()` — if the file appeared in `file_scores` (from search) but its chunks are missing from the batch fetch (race: file was deleted between search and chunk-fetch, or Store::get_chunks_by_origins_batch silently dropped a key), we synthesize a suggestion with empty `all_file_chunks`. The downstream `language` is `None` and `near_function` is "(top of file)". User sees an apparently-valid suggestion pointing at a file that no longer exists.
- **Suggested fix:** When `remove` returns `None`, log `tracing::debug!(file = %file.display(), "where_to_add: file in scores but no chunks fetched — skipping")` and `continue` rather than emit a malformed suggestion.

#### EH-V1.36-9: `cli/dispatch.rs:530` Err arm in router empty-suppression on per-cat alpha parse
- **Difficulty:** easy
- **Location:** src/search/router.rs:530
- **Description:** The match on `std::env::var(cat_key)` for per-category SPLADE alpha override has `Err(_) => {}` — meaning a missing env var is correctly silent, BUT a `VarError::NotUnicode` (env var contains invalid UTF-8) also falls into this arm and produces zero log output. An operator setting `CQS_SPLADE_ALPHA_NL=$'\xc3\x28'` (or pulling alpha from a Windows env with a stray BOM) gets no signal that their override was discarded.
- **Suggested fix:** `Err(std::env::VarError::NotPresent) => {}, Err(e) => tracing::warn!(var = %cat_key, error = %e, "Per-cat SPLADE alpha env var not unicode — ignored"),`. The triage notes EH-14 / dispatch.rs:208 already moved away from silent `.ok()`; this site never got the same treatment.

#### EH-V1.36-10: `train_data` skip-non-utf8 file uses bare `Err(_)` losing the actual decode error
- **Difficulty:** easy
- **Location:** src/train_data/git.rs:357
- **Description:** `Err(_) => { tracing::debug!(path, "Skipping non-UTF-8 file"); ... }` — the underlying `FromUtf8Error` carries the byte position of the first invalid sequence, which is useful for "is this a binary blob, a UTF-16 file, or a single curly-quote?" diagnostics. The bare `_` discards it. Files-skipped is a metric that can balloon to millions on a corpus mining run; debug log loses signal for "wait, I think these are actually UTF-16, why are we skipping them all?"
- **Suggested fix:** `Err(e) => { tracing::debug!(path, error = %e, valid_up_to = e.utf8_error().valid_up_to(), "Skipping non-UTF-8 file"); ... }`


<!-- ===== docs/audit-scratch/batch1-observability.md ===== -->
## Observability

#### `search_filtered` emits duplicate nested `info_span!("search_filtered", ...)`
- **Difficulty:** easy
- **Location:** src/search/query.rs:104 and src/search/query.rs:136
- **Description:** The public `pub fn search_filtered` at line 96 enters `info_span!("search_filtered", limit, threshold, rrf)` at line 104, then inside its body calls private `search_filtered_with_notes` at line 128 which enters *another* `info_span!("search_filtered", limit, rrf)` at line 136. Every user-facing search produces a span tree with two same-named "search_filtered" spans nested one inside the other. This pollutes flame graphs (the inner "search_filtered" span looks like recursion that isn't there), inflates the `tracing::Span` allocation count for every query (the hottest path in the daemon), and makes `journalctl` correlation noisier — operators see `search_filtered` twice per query without realising the inner one is redundant. Two callers exist for `search_filtered_with_notes`: `search_filtered` (which just entered) and `search_filtered_with_index` (path 689 enters its own `search_index_guided` span first, so a triple-nested duplicate fires there).
- **Suggested fix:** Drop the inner `info_span!` at line 136 (the outer one already covers the work) — or rename it to `search_filtered_inner` so the names disambiguate. Same review for `search_filtered_with_index` (line 667) which also delegates into `search_filtered_with_notes`.

#### `verify_hnsw_checksums` lacks an entry span on the integrity-check hot path
- **Difficulty:** easy
- **Location:** src/hnsw/persist.rs:121
- **Description:** `verify_hnsw_checksums` walks every file in the HNSW manifest, opens it, and BLAKE3-hashes the contents (potentially hundreds of MB). The function emits three `tracing::warn!` lines on IO error and returns an Err on mismatch, but it never opens an `info_span!`. Operators investigating "why is `cqs index` slow on startup" or "why did load fail" have no way to time the verify pass or see how many files it processed — a startup with a 500 MB HNSW blob spends seconds here invisibly. DS-V1.33-3 just hardened the missing-file branch (P1 fix) but didn't add the span. This sits adjacent to `pub fn save` and `pub fn load_with_dim`, both of which already have `info_span!`.
- **Suggested fix:** Add `let _span = tracing::info_span!("verify_hnsw_checksums", dir = %dir.display(), basename).entered();` at the top of the function. On Ok, emit a completion event with `files_verified` count and `elapsed_ms`.

#### `validate_summary` and `detect_all_injection_patterns` run untraced on every LLM result
- **Difficulty:** easy
- **Location:** src/llm/validation.rs:87, src/llm/validation.rs:152
- **Description:** Both functions run on every batch result the LLM provider returns (potentially thousands per `cqs index --improve-docs --improve-all` run). When validation rejects a summary — e.g. "prompt-injection pattern detected" — the call surface emits an `Err`/`Vec<&str>` but no `tracing::warn!` is fired with the offending pattern name and chunk identifier. Investigating "why did this batch's summaries get dropped" requires re-running with debug logging that doesn't exist. Compare to e.g. `embed_query` which emits cache-aware completion events.
- **Suggested fix:** Inside `validate_summary`, when `ValidationOutcome::Rejected` is returned, emit `tracing::warn!(reason = ?outcome, text_len = text.len(), "Summary validation rejected")`. In `detect_all_injection_patterns`, emit `tracing::debug!(pattern = pat, "Injection pattern matched")` per match. No span needed; these are leaf calls.

#### `compute_rewrite` lacks span on the doc-writer parse-resolve-apply hot path
- **Difficulty:** easy
- **Location:** src/doc_writer/rewriter.rs:242
- **Description:** `pub fn compute_rewrite` reads the source file from disk, runs tree-sitter, and applies edits. It is called by both `rewrite_file` (which has its own `info_span!` at line 290) and `write_proposed_patch` (line 530, also has a span). But `compute_rewrite` is a `pub fn` documented as the "pure parse-resolve-apply step shared by" both wrappers. External callers (or future callers) that hit this entry point directly bypass any span. The function does non-trivial work — file read + tree-sitter parse + N edit applications — and a span here would let operators see resolve failures (the silent "function not found in re-parse" branch noted in the doc-comment) at debug.
- **Suggested fix:** Add `let _span = tracing::info_span!("compute_rewrite", path = %path.display(), edit_count = edits.len()).entered();` at the top of `compute_rewrite`. The two wrapper spans become parent contexts when the wrappers are entered first; direct callers get coverage they currently lack.

#### `config.rs:582` emits redundant `eprintln!` next to a `tracing::warn!` for the same event
- **Difficulty:** easy
- **Location:** src/config.rs:582-594
- **Description:** When too many references are configured, the validator emits both an `eprintln!("Warning: ...")` AND a `tracing::warn!(count, max, "Too many references configured, truncating")` for the same event. The duplication is intentional ("warn the operator on the terminal") but it routes through two channels — the `eprintln!` lands in journald as unstructured stderr (since the daemon has no terminal), while the `tracing::warn!` lands as structured JSON. Operators get the same event twice with different shapes. The contract elsewhere in the codebase (see OB-V1.30.1-9 in `cli/watch/events.rs:190`) is "tracing only; the daemon has no TTY". This file pre-dates that rule.
- **Suggested fix:** Drop the `eprintln!` block at lines 582-588. The `tracing::warn!` already covers the operator-visibility need — `cqs watch` prints it via the configured subscriber, CLI runs see it because the default subscriber writes warns to stderr. Keep only `tracing::warn!`.

#### `find_ld_library_dir` failure mode is silent — no log when no LD_LIBRARY_PATH dir is selected
- **Difficulty:** easy
- **Location:** src/embedder/provider.rs:121
- **Description:** On Linux startup, `find_ld_library_dir` parses `LD_LIBRARY_PATH`, looks for a directory that ORT's symlink-providers logic should target, and returns `None` if none qualifies. The `None` path is hit when (a) `LD_LIBRARY_PATH` is unset (legitimate), or (b) every entry is empty/non-existent/the ORT cache itself (a misconfiguration). Both collapse to silent `None`. When CUDA provider load mysteriously fails downstream and the user reports "no GPU detected", there's no breadcrumb showing whether the LD-resolve step ran and what it saw.
- **Suggested fix:** Emit `tracing::debug!(ld_path = %ld_path, ort_lib_dir = %ort_lib_dir.display(), "Selected LD_LIBRARY_PATH dir for provider symlinks")` on the Some branch and `tracing::debug!(ld_path_set = !ld_path.is_empty(), entries = ld_path.matches(':').count() + 1, "No qualifying LD_LIBRARY_PATH entry for provider symlinks")` on the None branch.

#### `Embedder::token_count` and `token_counts_batch` lack spans on the tokenizer hot path
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:715, src/embedder/mod.rs:737
- **Description:** Both `pub fn` lack `info_span!`. They are called per-chunk during indexing and per-query during retrieval (token-count gating drives splits/window selection — see `split_into_windows`). Surrounding methods like `embed_documents`, `embed_query`, `warm`, `split_into_windows` all carry spans. Token counting is non-trivial work on a long input (full BPE/wordpiece tokenization), and when an indexing run is slow on large files, the operator currently can't see whether time is in token_count vs the actual ONNX forward pass.
- **Suggested fix:** Add `let _span = tracing::debug_span!("token_count", text_len = text.len()).entered();` to `token_count` and `let _span = tracing::debug_span!("token_counts_batch", count = texts.len()).entered();` to `token_counts_batch`. Debug-level (not info) because per-chunk in tight loops at info would flood journalctl.

#### `cagra_persist_enabled` logs once but skips chunk_count / dim context
- **Difficulty:** easy
- **Location:** src/cagra.rs:859-868
- **Description:** When `CQS_CAGRA_PERSIST=0` is set, the function emits `tracing::info!("CQS_CAGRA_PERSIST=0 — CAGRA persistence disabled")` exactly once (OnceLock). The log carries no fields. When operators set this var to debug a CAGRA load failure, they have no breadcrumb tying the disable-decision to which slot/index was loading at the time. Several call sites (`cagra_save`, `cagra_load`, `cagra_save_with_store`) all return early on `!cagra_persist_enabled()` without their own warn — silent skip of CAGRA persistence is exactly the kind of "looked done, did nothing" failure mode CLAUDE.md memory flags.
- **Suggested fix:** Inside each `if !cagra_persist_enabled()` early-return arm at lines 890, 985, 1091, emit `tracing::warn!(path = %path.display(), "CAGRA op skipped — CQS_CAGRA_PERSIST=0")`. The OnceLock startup line is fine; the per-call warn is what a debugging operator actually needs.

#### `splade::probe_model_vocab` has a span but no completion event with elapsed_ms / vocab_size
- **Difficulty:** easy
- **Location:** src/splade/mod.rs:127
- **Description:** `probe_model_vocab` enters a `debug_span!("probe_model_vocab", path = ...)` but emits no completion event tying together how long the probe took and what vocab_size it read. The probe involves loading an ONNX model header — non-trivial IO + parse — and is called once per encoder construction. When SPLADE startup is slow, operators currently can't tell whether the probe was the bottleneck. Compare to `embed_documents` (line 877) which emits a completion `tracing::info!` with `elapsed_ms`, `total`, `dim`.
- **Suggested fix:** Capture `let started = std::time::Instant::now();` at the span entry and emit `tracing::debug!(vocab_size = ?probed, elapsed_ms = started.elapsed().as_millis() as u64, "probe_model_vocab complete")` on the Ok return path.

#### `worktree.rs::record_worktree_stale` and `is_worktree_stale` lack tracing on staleness state changes
- **Difficulty:** easy
- **Location:** src/worktree.rs:219, src/worktree.rs:229
- **Description:** Both functions touch the cross-worktree staleness flag (a marker file or atomic) but emit no tracing. `record_worktree_stale` is the producer side ("our worktree's index is now stale"); `is_worktree_stale` is the consumer side ("should I show a warning to the user"). When a multi-worktree workflow misbehaves — agent A's reindex doesn't propagate the stale flag, agent B keeps serving stale results — there's no journal trail to diagnose which side dropped the signal. Note CLAUDE.md memory specifically calls out worktree leakage (#1254) as a pain point; observability here is exactly what would make that diagnosable.
- **Suggested fix:** Add `tracing::info!(worktree_root = %worktree_root.display(), "worktree marked stale")` at the producer (`record_worktree_stale`) and `tracing::debug!(stale = res, "is_worktree_stale check")` at the consumer.


<!-- ===== docs/audit-scratch/batch1-test-coverage-adversarial.md ===== -->
## Test Coverage (adversarial)

#### Sparse-vector weight NaN/Inf round-trips through SQLite untested
- **Difficulty:** easy
- **Location:** src/store/sparse.rs:138 — `upsert_sparse_vectors` / `load_all_sparse_vectors:381`
- **Description:** `upsert_sparse_vectors` and `load_all_sparse_vectors` accept and emit `f32` weights with no `is_finite()` guard. SQLite's REAL type happily stores NaN/Inf as a regular bit-pattern, and the loader casts `weight: f64 → as f32` without checking. Existing tests (`test_sparse_roundtrip`, `test_sparse_upsert_replaces`, `test_fk_cascade_removes_sparse_rows_on_chunk_delete`) all use finite, hand-picked weights. A SPLADE encoder hiccup, a corrupted sparse cache row, or an external tool writing into `sparse_vectors` could leak NaN weights, which then propagate through downstream sparse-cosine math (and `f32::NAN > anything` is `false`, so a NaN-weighted chunk silently drops out of every result instead of being clamped or rejected). Mirrors P2-12 (umap NaN, ✅ #1333) but for SPLADE.
- **Suggested fix:** Add two tests in `src/store/sparse.rs` tests module: (1) `upsert_sparse_vectors` with `(token_id, f32::NAN)` and `(token_id, f32::INFINITY)` — pin the contract (reject vs. clamp vs. silently store) and add a runtime guard returning `StoreError::InvalidInput` if reject is the chosen contract; (2) `load_all_sparse_vectors` after a manual `INSERT INTO sparse_vectors VALUES (..., 'nan')` via raw sqlx, to verify the loader either filters or surfaces the row, not silently produces a NaN-weighted vector.

#### `parse_aspx_chunks` malformed/unterminated `<%...%>` block has no test
- **Difficulty:** easy
- **Location:** src/parser/aspx.rs:44 (`CODE_BLOCK_RE`) → src/parser/aspx.rs:141 (`find_code_blocks`)
- **Description:** The CODE_BLOCK_RE pattern `<%(=|:|@|--|--)?(.*?)(--%>|%>)` is non-greedy but on input where the closing `%>` is missing it scans forward through the whole file before failing the alternation. Same DoS shape as `L5K_ROUTINE_BLOCK_RE` (which has SEC-8 acknowledged in comments and an `unterminated_routine_no_panic` test at l5x.rs:782) — but aspx has no equivalent test. Adversarial input: a 10 MB `.aspx` containing many `<%` openers and no closers. The regex crate's linear-time guarantee prevents catastrophic backtracking, but the constant factor on a 50 MB file with 1k unterminated blocks is still measurable.
- **Suggested fix:** Add `parse_aspx_unterminated_code_block_no_panic` and `parse_aspx_truncated_at_open_tag` tests in `src/parser/aspx.rs` mod tests. Feed `"<html><% Response.Write(\"never closes\""` and assert `parse_aspx_chunks` returns `Ok(_)` without panic and within bounded time. Also pin behaviour for `<script runat="server">` with no `</script>` (currently SCRIPT_BLOCK_RE silently drops the unmatched opener — pin that too).

#### `Embedder::split_into_windows` adversarial token boundaries untested
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:782 — `split_into_windows`
- **Description:** The function has one happy-path test (`split_into_windows_preserves_original_text` at line 2432) and only that one. Untested edges that have explicit handling code:
  (1) `max_tokens == 0` early-returns `Ok(vec![])` (line 788);
  (2) `overlap >= max_tokens / 2` returns an error (line 795);
  (3) `char_end <= char_start` collapse (line 854) — fires on tokens whose offsets are `(0, 0)` for added special tokens; the fallback returns the full text;
  (4) text containing a 4-byte multibyte codepoint at exactly the window boundary so `char_start..char_end` would split mid-codepoint (tokenizer offsets are byte offsets, but Rust string slicing panics on non-char boundaries — relies on the tokenizer never returning intra-codepoint offsets).
  Any of (1)-(4) regressing would be silent for short inputs and only break on exact boundary conditions.
- **Suggested fix:** Add four targeted tests: `split_into_windows_max_tokens_zero_returns_empty`, `split_into_windows_overlap_too_large_errors`, `split_into_windows_collapsed_offsets_falls_back_to_full_text` (synthesise a fake encoding via a mock tokenizer or pick text where padding tokens inject `(0,0)`), `split_into_windows_emoji_at_window_boundary` (emoji-heavy input with `max_tokens` chosen to land the window edge mid-grapheme — assert no panic).

#### `sanitize_fts_query` property tests miss `{` and `}` strip set
- **Difficulty:** easy
- **Location:** src/store/mod.rs:1532 (property tests) vs. src/store/mod.rs:203 (production strip set)
- **Description:** The production `sanitize_fts_query` strips `'"' | '*' | '(' | ')' | '+' | '-' | '^' | ':' | '{' | '}'` (10 chars), but every property test (`prop_sanitize_no_special_chars`, `prop_pipeline_safe`, `prop_sanitize_all_special`, `prop_sanitize_adversarial`) only asserts the first 8 are absent — `{` and `}` are not in the property assertion set or in the `prop::sample::select` input universe. If a refactor accidentally drops `{`/`}` from the strip set, no test fires. FTS5 doesn't error on `{`/`}` literally but they're in the strip set for a reason (column filter syntax `{col}: term`) — losing the strip silently re-enables column-filter injection in a query that was previously sanitized.
- **Suggested fix:** Update the four property tests to include `{` and `}` in (a) the negative-assertion `matches!` set and (b) the `prop::sample::select` adversarial-input vector. One-line edit per test plus a regression test `sanitize_strips_curly_braces` with a hand-picked input like `{path}:foo bar`.

#### Daemon JSON-RPC surface: invalid UTF-8 surrogate halves in args untested
- **Difficulty:** medium
- **Location:** src/cli/watch/adversarial_socket_tests.rs (8 cases) + src/cli/watch/socket.rs:60 — `handle_socket_client`
- **Description:** Existing adversarial tests cover oversized line, trailing garbage, UTF-16 BOM, bare newline, missing command, non-string args, 500 KB arg, and NUL byte. Two remaining adversarial JSON shapes are not pinned: (a) a JSON string containing an unpaired surrogate `"\uD800"` (serde_json 1.0 *accepts* lone surrogates by default and emits a `String` containing `WTF-8`-shaped bytes, which then flows into `dispatch_via_view` — downstream `to_string()` works but any path that crosses an FFI/SQLite boundary may hit issues); (b) deeply-nested JSON like `{"command":"ping","args":[],"x":[[[[[…]]]]]}` (1000 levels) — serde_json default has no recursion limit, can stack-overflow in the parser thread which `catch_unwind` may not catch (SIGSEGV from stack guard page is not a Rust panic).
- **Suggested fix:** Add two tests to `src/cli/watch/adversarial_socket_tests.rs`: `daemon_handles_lone_surrogate_in_string_arg` (assert command is rejected or runs cleanly, no panic, no half-open socket) and `daemon_rejects_deeply_nested_json` (assert: either parser refuses with a structured error, or the daemon thread doesn't take down the whole daemon — handler thread isolation contract). For the recursion case, `serde_json::de::Deserializer::with_recursion_limit(128)` would be the production fix.

#### Daemon socket: zero concurrent-connection / queue-saturation tests
- **Difficulty:** medium
- **Location:** src/cli/watch/socket.rs:45 — `max_concurrent_daemon_clients` and the accept loop in src/cli/watch/daemon.rs
- **Description:** `max_concurrent_daemon_clients()` reads `CQS_DAEMON_MAX_CLIENTS` (default presumably 16-32) and the accept loop is supposed to bound parallel handler threads. Zero tests fire `N+1` simultaneous clients to verify the (N+1)th gets queued, rejected, or admitted — nor that a wedged client (sends partial line, then sleeps) doesn't pin a slot forever (the read_timeout helps but the test doesn't exist). On a daemon servicing N=4 agents this matters: a single hung agent can take down the daemon for the others.
- **Suggested fix:** Add `daemon_caps_concurrent_clients` to `adversarial_socket_tests.rs` — open `max_concurrent + 5` `UnixStream`s, each writing a slow/partial request, and assert that the daemon either queues them, rejects with `too_many_clients`, or honours the read_timeout. Also `daemon_slow_client_does_not_starve_others`: hold one connection open silently, fire a fast valid request through a second connection, verify it completes within 1s.

#### `build_hierarchy` BFS cycle handling untested
- **Difficulty:** easy
- **Location:** src/serve/data.rs:673 — `build_hierarchy`
- **Description:** Existing tests (`build_hierarchy_walks_callees_to_depth`, `hierarchy_extreme_depth_is_clamped`) verify max_depth clamping and basic walk, but not cyclic call graphs (mutual recursion: `a→b, b→a`). The current code uses `depth_by_name.contains_key` to avoid revisiting, which *should* prevent infinite loops, but a regression that switched to "always insert with min depth" would loop forever on cycles. The `serve` HTTP path is exposed to localhost — an attacker on the same host could find a known recursive call pair and DoS the daemon.
- **Suggested fix:** Add `build_hierarchy_handles_mutual_recursion` test in `src/serve/tests.rs` — seed two chunks `a` and `b` with `a→b` and `b→a` call edges, request hierarchy from `a` with `max_depth=10`, assert response contains exactly `{a, b}` with `bfs_depth = {0, 1}` and the call completes in <100ms. Also `build_hierarchy_handles_self_call` (`a→a`).

#### Concurrent writer contention path: zero coverage
- **Difficulty:** hard
- **Location:** src/store/mod.rs:937 (busy_timeout config), src/cli/watch/reindex.rs (writer), src/cli/commands/index/build.rs (writer)
- **Description:** The store relies on SQLite WAL + `busy_timeout(30s)` (recently bumped from 5s in #1450) + an in-process `WRITE_LOCK` mutex to serialise writers. Tests cover concurrent *readers* (`stress_test::test_concurrent_searches`, ignored by default) and verify `busy_timeout_from_env`, but not the actual contention scenario: two writer threads — one running `cqs index` long-batch, one running `cqs notes add` mutation — both reaching `begin_write()`. The interaction between in-process `WRITE_LOCK` and SQLite's BUSY response on the WAL writer lock is the flakiest part of the system on WSL (per memory comments) and has no regression test. A subtle change that drops or shortens the in-process lock could let two `BEGIN IMMEDIATE` collide and surface BUSY beyond the busy_timeout.
- **Suggested fix:** Add `tests/store_concurrent_writers_test.rs` (gated `#[ignore]` if too slow, runnable in CI nightly): spawn 2 threads, each upserting 100 chunks for 5 seconds against a shared `Arc<Store>`, assert both complete with no `database is locked` error and with the union of inserted chunks visible. Then a second test mixing `upsert_chunks_batch` with `upsert_notes_batch` — different write paths must serialise correctly.

#### Embedding pipeline: NaN/Inf escape from `embed_documents` untested
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:877 — `embed_documents` / `embed_batch:1157`
- **Description:** `Embedding::try_new` rejects NaN/Inf at the constructor, but `embed_batch` constructs Embeddings via `Embedding::new` (the unchecked constructor — see line 121 doc: "**Prefer `try_new()` for untrusted input**"). If ONNX inference produces NaN/Inf for *any* reason (bad ONNX op, dim-zero quirk, cuDNN driver bug, `--features ep-coreml` weirdness), the Embedding sails through to HNSW insert, where it pollutes the search index. The `cache.rs` writer at line 639 *does* check `!f.is_finite()` before writing to disk cache — but the in-memory hot path doesn't. Tests cover `try_new` rejection but not "embed_documents output is finite" — no test confirms inference output finiteness.
- **Suggested fix:** Add `embed_documents_output_is_finite` and `embed_query_output_is_finite` integration tests in `tests/embedding_test.rs` — call against a fixed mock model and assert `result.iter().all(|e| e.as_slice().iter().all(|v| v.is_finite()))`. Cheap regression-catcher; would have caught any future ONNX-runtime upgrade that started leaking subnormals as NaN.

#### `serve` HTTP handlers: unicode/zero-width/control chars in `chunk_id` path param
- **Difficulty:** medium
- **Location:** src/serve/handlers.rs:172 (`chunk_detail`), src/serve/handlers.rs:244 (`hierarchy`)
- **Description:** Chunk IDs flow from a URL `Path(id): Path<String>` straight into SQL `WHERE id = ?` (parameterised — no SQL-injection risk) but no test pins behaviour for adversarial path parameters: zero-width joiner (`‍`), RTL override (`‮`), NUL byte (`%00`), 100 KB id (URL-encoded), URL-encoded `../../etc/passwd`, percent-decoded surrogate. axum/tower decode the path before it lands in the handler; a refactor swapping `Path<String>` for `Path<RawString>` or adding any post-decode normalisation would silently change behaviour. Existing `chunk_detail_unknown_id_returns_404` only tests a normal-ASCII unknown id. Adversarial IDs in tracing logs are also a concern — the chunk_id is logged at info level (`tracing::info!(chunk_id = %id, "serve::chunk_detail")`) and an RTL override there can flip log lines in journalctl.
- **Suggested fix:** Add `chunk_detail_handles_adversarial_unicode_id` and `hierarchy_handles_oversized_id_path` tests in `src/serve/tests.rs`. Each fires an `axum::test::TestRequest` with the adversarial id and asserts: HTTP 404 (or 400 for clearly-malformed) with no panic, log line is bounded length, no half-open response. Also add a 64 KB id test to pin "what's the URL length cap" (axum's default is server-config-dependent).

#### `parse_env_f32` rejects NaN but `parse_env_usize_clamped` zero-input UB untested
- **Difficulty:** easy
- **Location:** src/limits.rs:269 — `parse_env_usize_clamped`
- **Description:** Existing tests cover above-max, below-min, garbage, and missing — but the docstring says "Missing/zero/garbage falls back to `default` (also clamped)", and the implementation at line 273 reads `Ok(n) if n > 0 => clamp(n)` — meaning `n=0` falls through to the `_ =>` arm, which `clamp(default)`. If a future refactor moves the `n > 0` check, a caller passing `min=1, max=100, default=0` would silently get 0 (not clamped to min=1), which would then cause divide-by-zero in `embed_batch_size` math. Worth pinning given how widely this helper is used (RT-RES limits, sparse batch sizes, daemon timeouts).
- **Suggested fix:** Add `parse_env_usize_clamped_zero_input_uses_clamped_default` test asserting `parse_env_usize_clamped("CQS_TEST_KEY_DOES_NOT_EXIST", 0, 1, 100) == 1` (not 0) — which forces the `default.clamp(1, 100)` branch to actually fire. Also `parse_env_usize_clamped_default_below_min` to pin the contract on misconfigured-default callers.


<!-- ===== docs/audit-scratch/batch1-robustness.md ===== -->
## Robustness

#### RB-V1.36-1: `doc_writer::compute_rewrite` / `rewrite_file` read source files unbounded
- **Difficulty:** easy
- **Location:** src/doc_writer/rewriter.rs:251 and src/doc_writer/rewriter.rs:319
- **Description:** Both `compute_rewrite` and `rewrite_file` call `std::fs::read_to_string(path)` with no size guard. They are reachable from `cqs --improve-docs` / `--improve-all` which iterates every project file. A pathological repo (giant generated/SQL/JSON file or symlink to /dev/zero on Linux) drives unbounded heap allocation. The parser path uses `CQS_PARSER_MAX_FILE_SIZE` and the file-read path in `cli/commands/io/read.rs` uses `CQS_READ_MAX_FILE_SIZE`; this site has neither.
- **Suggested fix:** Stat first and bail out (or skip with warn) when `meta.len() > CQS_DOC_WRITER_MAX_FILE_SIZE` (default e.g. 4 MB — the rewriter only ever touches source files small enough for the parser to have ingested them). Wire the same `metadata.len() > max_bytes → bail!` block already used in `convert/mod.rs::read_to_string_with_size_limit`.

#### RB-V1.36-2: `cli::commands::search::query` parent-context read uncapped
- **Difficulty:** easy
- **Location:** src/cli/commands/search/query.rs:899
- **Description:** When parent chunk is missing from the DB, the code falls back to `std::fs::read_to_string(&canonical)` to extract a line range. No size cap. Path is canonicalized + root-restricted (good), but the file itself is still read whole into memory just to pull `lines[start..end]`. A 5 GB file in the project tree (e.g., extracted dataset, debug log) loaded once per parent-context fetch will OOM the search process.
- **Suggested fix:** Either (a) check `metadata.len()` first and skip with a `tracing::warn!` if above a small cap (~4 MB), or (b) replace with line-bounded read using `BufReader::lines().take(line_end + 1)` and discard everything before `start`.

#### RB-V1.36-3: `cli::commands::infra::hook` reads existing git hooks unbounded
- **Difficulty:** easy
- **Location:** src/cli/commands/infra/hook.rs:183, :365, :506
- **Description:** Three sites (`do_install`, `do_uninstall`, `do_status`) read every managed git hook file via `std::fs::read_to_string(&path)` with no size guard. A foreign hook truncated to `/dev/zero` or a multi-GB hook file (rare but possible — corruption, malicious commit hook injection on a shared CI box) will OOM `cqs ci ...`. Less likely than RB-V1.36-1 but no defense at all.
- **Suggested fix:** Stat first; cap at e.g. 1 MB (hooks are normally <10 KB). On overflow, treat as "foreign hook" (so install/uninstall is conservative) and warn. Only `contains(HOOK_MARKER_PREFIX)` is needed — that fits in the first few KB if it's a managed hook, so a streaming `BufReader::read_until(b'\n')` over the first 64 KB is enough.

#### RB-V1.36-4: `slot::ensure_slot_config` reads slot.toml unbounded
- **Difficulty:** easy
- **Location:** src/slot/mod.rs:339
- **Description:** Slot config bootstrap calls `fs::read_to_string(&final_path)` with no size cap. The `Config::load_file` path *does* enforce `MAX_CONFIG_SIZE` (config.rs:720) — slot.toml uses the same TOML parser shape but skips the guard entirely. Less critical than the doc-writer path because slot.toml is owned by cqs, but a hand-edited 10 GB slot.toml triggers OOM rather than the documented "warn + reset to default" path on the line below.
- **Suggested fix:** Mirror the `Config::load_file` size check (read meta first, bail/warn at MAX_CONFIG_SIZE). Same pattern, copy-paste with the slot path.

#### RB-V1.36-5: `store::chunks::staleness::compute_fingerprint` blake3 reads file whole
- **Difficulty:** medium
- **Location:** src/store/chunks/staleness.rs:161
- **Description:** `std::fs::read(path)` followed by `blake3::hash(&bytes)`. No size cap — runs during watch-driven staleness checks. The parser path skips files above `CQS_PARSER_MAX_FILE_SIZE`, but staleness can still be invoked on the same path before the size check downstream (or after a file grows post-index). For a 5 GB SQL dump that grew between index and watch, the watch reload tries to fingerprint it whole.
- **Suggested fix:** Switch to `blake3::Hasher::new()` + `Read` chunked into 64 KB. Same hash output, bounded RSS. Apply the same change at `cli/watch/reindex.rs:662` which also reads-then-hashes.

#### RB-V1.36-6: `train_data::diff::find_changed_functions` `usize` add without saturating
- **Difficulty:** easy
- **Location:** src/train_data/diff.rs:139
- **Description:** `let hunk_end = h.new_start + h.new_count.saturating_sub(1);` — only the inner subtraction is saturating; the outer add is bare. `parse_hunk_header` (line 50/58) parses raw `usize` from the diff header `@@ -a,b +c,d @@`. An attacker-supplied (or `git diff`-emitted-on-corrupt-pack) header `@@ -1,1 +18446744073709551615,2 @@` parses to `new_start = usize::MAX`, `new_count = 2`. `usize::MAX + 1` panics in debug; wraps to 0 in release, which then makes `hunk_end >= func.start_line` accidentally false-negative for every function. Reachable from `cqs review` / `affected` whenever the user pipes diff content through.
- **Suggested fix:** `h.new_start.saturating_add(h.new_count.saturating_sub(1))`. Or reject hunk headers where `new_start > i64::MAX as usize` at parse time (no real diff has line numbers > 2^63).

#### RB-V1.36-7: `where_to_add::compiled_import_regexes` mutex `expect` propagates panics
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:773 and :793
- **Description:** Both lock acquires use `.expect("compiled_import_regexes mutex poisoned")`. If any panic happens while holding the lock (regex compile that recurses into another panic, allocator failure inside `HashMap::get`), every subsequent `cqs where`/`task` call panics permanently for the lifetime of the process — the daemon can't recover without restart. The cache only stores regex Arcs; it's safe to recover from poison.
- **Suggested fix:** Replace both with `.unwrap_or_else(|e| e.into_inner())` (the same pattern already used at `embedder/provider.rs:512` for `ENV_LOCK`). Poison is recoverable here because the cache state isn't invariant-bearing — worst case we recompile a regex.

#### RB-V1.36-8: `language::Language::def` panics on disabled feature flag at runtime
- **Difficulty:** medium
- **Location:** src/language/mod.rs:1113
- **Description:** `Language::def()` calls `try_def()` and unconditionally panics with "Language '...' not in registry — check feature flags" when the registry lookup returns None. `try_def()` exists exactly to support this fallible path, but `def()` is called in many production sites that don't gate on the feature being enabled. If a stored chunk references a `Language` whose feature was compiled out (legitimately possible after a rebuild without `--features all-langs`), the next search/parse panics. The doc comment even calls this out as a deliberate panic but the production call sites don't all guard.
- **Suggested fix:** Audit `def()` callers — switch any non-test caller that touches stored data to `try_def()` and route through `LanguageError`. The bare panic should remain only on hard-coded compile-time language references.

#### RB-V1.36-9: `print_telemetry_text` divide-by-zero when `total == 0` but commands have entries
- **Difficulty:** easy
- **Location:** src/cli/commands/infra/telemetry_cmd.rs:477
- **Description:** Triage v1.33.0 fixed P1-25 (sessions divisor in `format_sessions_line`, line 434), but the same file still has `let pct = (count as f64 / total as f64) * 100.0;` at line 477 where `total = output.events`. If telemetry events are filtered/empty but the commands map carries a stale 0-count entry, `total = 0` and `count` could be 0 → produces NaN, which clippy/serde would format as `NaN%`. Less likely than the sessions case (the canonical path empties commands when total=0) but the guard is one-sided.
- **Suggested fix:** Mirror the `if sessions > 0` guard from line 435: wrap the command-frequency loop in `if total > 0 { ... }`, or compute `pct` as `if total > 0 { count*100/total } else { 0 }`.

#### RB-V1.36-10: `store::sparse::token_dump_paged` casts `f64` weight to `f32` without finite check
- **Difficulty:** easy
- **Location:** src/store/sparse.rs:400
- **Description:** `current_vec.push((token_id as u32, weight as f32))` reads `weight: f64` straight from SQLite. `cache.rs:639` already filters NaN/Inf at insert time for the embedding cache, but the sparse `weight` column has no such guard at read time. A hand-edited DB or a bug in the splade write path that writes Inf (matching the `test_raw_logits_positive_inf_passes_through_as_inf_weight` test that's still passing — see splade/mod.rs:1565) lands in every downstream BM25-style scorer as `f32::INFINITY`, which corrupts every sort that compares NaN-via-Inf+0.
- **Suggested fix:** Add `if !weight.is_finite() { tracing::warn!(...); continue; }` before the push. Cheap; consistent with the cache write path's policy.


<!-- ===== docs/audit-scratch/batch1-scaling.md ===== -->
## Scaling & Hardcoded Limits

#### SHL-V1.36-1: `CQS_MAX_CONNECTIONS` default `unwrap_or(4)` ignores host parallelism
- **Difficulty:** easy
- **Location:** src/store/mod.rs:734-737
- **Description:** SQLite pool size defaults to 4 regardless of CPU count. On a 32-core host running `cqs serve`, every concurrent request contends for one of 4 connections — and `serve_blocking_permits()` (limits.rs:348) clamps spawn-blocking permits to this same `max_connections`, so the entire serve concurrency budget is fixed at 4 even though `available_parallelism()` reports 32. This is the same anti-pattern the v1.33 audit (SHL-V1.33-10) fixed in `project.rs::search_across_projects` and `reference.rs::load_references` (one of which still has the bug — see SHL-V1.36-2). The store has the most impact because it gates every other concurrency limit downstream of it.
- **Suggested fix:** Default to `available_parallelism().min(8)` (matching the v1.33 fix to `project.rs:260`). Power users on big hosts get linear scaling; small hosts stay capped. `CQS_MAX_CONNECTIONS` env override still wins verbatim.

#### SHL-V1.36-2: `reference.rs::load_references` still uses `unwrap_or(4)` after v1.33 fix to project.rs
- **Difficulty:** easy
- **Location:** src/reference.rs:208
- **Description:** v1.33 (SHL-V1.33-10) replaced the `unwrap_or(4)` rayon thread-count fallback in `project.rs:260` with `available_parallelism().get().min(8)`, citing 32-core hosts being under-utilized. The sibling site at `reference.rs:208` uses the same `CQS_RAYON_THREADS` env var with the same `unwrap_or(4)` pattern and was missed. Both sites read identical env vars and serve the same purpose (parallel store+HNSW load) — they should match. Comment at `project.rs:247` explicitly mentions matching `watch/runtime.rs:62-66`'s pattern; reference.rs got skipped.
- **Suggested fix:** Copy the v1.33 fallback closure verbatim from `project.rs:260-265` to `reference.rs:208`. Single-line edit.

#### SHL-V1.36-3: `BRUTE_FORCE_BATCH_SIZE = 5000` doesn't scale with model dim
- **Difficulty:** medium
- **Location:** src/search/query.rs:197
- **Description:** Cursor-based brute-force search loads 5000 chunks/iteration regardless of embedding dimension. At BGE-large (1024-dim, f32) that's ~20 MB per batch — fine. At Qwen3-4B (4096-dim) it's ~80 MB; at a hypothetical 8192-dim model it's ~160 MB held in `Vec<u8>` per row plus the bounded heap. The constant is private, not env-overridable, and the comment explicitly cites "bounds memory" as the rationale — but the bound now scales with model choice, not the constant. Same issue applies to the inline `5000`-row paginator in `EmbeddingBatchIterator` referenced in the comment.
- **Suggested fix:** Compute `batch_size = (TARGET_BYTES / (dim * 4)).clamp(500, 50_000)` with `TARGET_BYTES = 20 * 1024 * 1024` (~20 MB), or read `CQS_BRUTE_FORCE_BATCH_SIZE` env var. Pass the embedder's `dim` through `search_unified_with_index` callers (already in scope via `query.len()`).

#### SHL-V1.36-4: `HNSW_BATCH_SIZE = 10000` default doesn't scale with embedding dim
- **Difficulty:** easy
- **Location:** src/cli/commands/index/build.rs:1232-1238
- **Description:** HNSW insert batch defaults to 10000 vectors regardless of dim. At 1024-dim (BGE-large) that's 40 MB; at 4096-dim (custom Qwen3-style) it's 160 MB held in the per-batch `Vec`. `lib.rs:489` documents this as a fixed constant without flagging the dim coupling. `embed_batch_size_for(model)` (`src/embedder/models.rs:792`) already implements the right pattern (scale by `1024.0/dim`); HNSW build should mirror it.
- **Suggested fix:** Have `hnsw_batch_size()` accept the embedder's `dim` and scale: `(10_000 * 1024 / dim).clamp(500, 50_000)`. Env override unchanged.

#### SHL-V1.36-5: `cagra_stream_batch_size()` default 10000 doesn't scale with dim
- **Difficulty:** easy
- **Location:** src/cagra.rs:166 (function body)
- **Description:** Sister problem to SHL-V1.36-4. `cagra_stream_batch_size()` returns env or 10_000 verbatim — same dim-blind constant as the HNSW path. P3-15 in v1.33 triage (✅ #1363) addressed CAGRA `build_from_store` at the inner `BATCH_SIZE` level, but the `cagra_stream_batch_size()` resolver itself still ships a fixed default. The streaming path (cagra.rs:747 `Vec::with_capacity(chunk_count * dim)`) already shows dim-awareness in adjacent code — the inconsistency is the smell.
- **Suggested fix:** Same scaling treatment: `parse_env_usize_clamped("CQS_CAGRA_STREAM_BATCH_SIZE", scaled_default(dim), 500, 50_000)` where `scaled_default(dim) = (10_000 * 1024 / dim).max(500)`.

#### SHL-V1.36-6: `DEFAULT_RERANKER_BATCH = 32` and `splade_batch_size()` = 32 ignore model dim/seq
- **Difficulty:** medium
- **Location:** src/reranker.rs:41 ; src/cli/watch/reindex.rs:202
- **Description:** Two aux-model batch defaults are hardcoded to 32. Reranker comment explicitly says "32 is conservative because cross-encoder runs produce larger activations than plain encoder forward passes" — fine for ms-marco-MiniLM (384-hidden, 512-seq, ~100 MB activations at 32). But the Phase A model registry now supports custom rerankers and SPLADE variants; the default doesn't scale with hidden_size or max_seq_len. Same problem `embed_batch_size_for(model)` solves for the encoder. Result: small model → unused VRAM; large model → OOM at the documented default.
- **Suggested fix:** Mirror `ModelConfig::embed_batch_size`. Pull `hidden_size` and `max_seq_length` from the loaded reranker / SPLADE model and apply `32 * (384/hidden) * (512/seq)` rounded to a power-of-two, clamped `[1, 256]`. Env vars (`CQS_RERANKER_BATCH`, `CQS_SPLADE_BATCH`) still win.

#### SHL-V1.36-7: stale comment in `lib.rs` cites pre-3.32.0 SQLite 999 host-param limit
- **Difficulty:** easy
- **Location:** src/lib.rs:471-492
- **Description:** The "Batch Size Constants" doc-comment block in `lib.rs` says: "SQLite limit: max 999 bind parameters per statement. A query with N columns per row can batch `floor(999 / N)` rows." This is the legacy pre-2020 limit. Modern SQLite (3.32.0+, what `sqlx` ships) supports 32766, and `store/helpers/sql.rs:26` (`SQLITE_MAX_VARIABLES = 32766`) plus the `max_rows_per_statement(N)` helper already document the new ceiling. The lib.rs block lists the old "common sizes" (500/200/132/100) — readers using this as a guide for new code will pick numbers ~65× too small. This is the same documentation-vs-code drift that v1.33 found across P1-35..P1-39 (all fixed in #1324 by switching to `max_rows_per_statement`); the documentation block was missed.
- **Suggested fix:** Replace the block with a pointer to `store/helpers/sql.rs::max_rows_per_statement` and delete the stale 999/floor(999/N) explanation. New code should consult the helper, not the comment block.

#### SHL-V1.36-8: `EMBED_CHANNEL_DEPTH = 64` default ignores embedding dim
- **Difficulty:** easy
- **Location:** src/cli/pipeline/types.rs:115-129
- **Description:** Pipeline channel buffers up to 64 `EmbeddedBatch` messages. Each batch holds `embed_batch_size_for(model)` chunks × `dim` × 4 bytes of embeddings. At default BGE-large (batch=64, dim=1024) that's 64 × 64 × 1024 × 4 = 16 MB peak buffered. At Qwen3-style 4096-dim with batch=16 (post-scaling) it's 64 × 16 × 4096 × 4 = 16 MB — coincidentally similar. But the `64` default was picked when batch was a fixed 64; with `embed_batch_size_for` already dim-aware, the channel depth should be too: a model that scales batch *down* probably wants channel depth *up* (more parallelism since each msg is smaller), not unchanged.
- **Suggested fix:** Document the coupling at minimum (current doc says "smaller to bound memory" without mentioning dim). Better: scale depth inversely with batch — e.g. `64 * (default_batch / actual_batch)` clamped `[16, 256]`, or simply pin the *byte budget* (`32 MB / msg_bytes`) and derive depth.

#### SHL-V1.36-9: `DEFAULT_QUERY_CACHE_SIZE = 128` doesn't scale with available memory or dim
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:337
- **Description:** Query embedding LRU caches 128 entries regardless of dim. Comment says "~4 KB/entry at 1024-dim, ~3 KB/entry at 768-dim" — total ~512 KB. Fine for default models, but: (1) at 4096-dim it's ~16 KB/entry / 2 MB total — still fine but the rationale flips; (2) on a daemon serving an agent fleet the hit rate is highly dependent on size, and 128 entries is a coin toss for a 30-task batch loop hitting `cqs scout` for each task. Memory is cheap; cache misses are 50-200 ms per query. The default is too conservative for the daemon use case.
- **Suggested fix:** Bump default to 1024 (still ~4 MB at 1024-dim, trivial) or tier on `cqs serve`/`cqs watch --serve` vs CLI one-shot. Env var `CQS_QUERY_CACHE_SIZE` already exists as escape hatch.

#### SHL-V1.36-10: `MAX_CONCURRENT_DAEMON_CLIENTS = 16` is fixed regardless of host
- **Difficulty:** easy
- **Location:** src/cli/watch/socket.rs:40
- **Description:** Daemon caps concurrent clients at 16 with comment "matches typical agent fan-out... ~32 MB worst-case stack". The env override `CQS_MAX_DAEMON_CLIENTS` exists, but the *default* doesn't scale with cores or RAM. On a 64-core host running an agent swarm with 32+ parallel `cqs scout` calls, requests queue serially past 16. Stack memory isn't the binding constraint on modern 64-bit hosts (16 × 2 MB = 32 MB is rounding error); the cap was sized for "typical agent fan-out" of an unspecified era. With Claude Code Tasks-via-agents the typical fan-out is now 5-30.
- **Suggested fix:** Default `available_parallelism().get().clamp(16, 64)`. Keeps small hosts at 16, scales 32-core hosts to 32. Env override still wins.


<!-- ===== docs/audit-scratch/batch2-algorithm.md ===== -->
## Algorithm Correctness

#### `extract_file_from_chunk_id` mis-parses window indices ≥ 100
- **Difficulty:** easy
- **Location:** src/search/scoring/filter.rs:44-53 (the `wN` arm of `is_window_suffix`)
- **Description:** `is_window_suffix` accepts `wN` only when the suffix length is `≤ 3` (so `w0`..`w99`). `apply_windowing` (`src/cli/pipeline/windowing.rs:65`) and the legacy chunker (`src/cli/pipeline/mod.rs:483`) format ids as `format!("{}:w{}", parent_id, window_idx)` where `window_idx` is `u32` and uncapped. A chunk whose tokenized length needs ≥ 100 windows therefore produces an id ending in `:w100` (length 4), which `is_window_suffix` rejects. `extract_file_from_chunk_id` then strips only the standard `(line, hash)` pair — the result is `path:line:hash` instead of `path`, corrupting every downstream consumer that joins by file or de-dups across windows: SPLADE fusion `dense_results`/`sparse_results` ID set, `extract_file_from_chunk_id` callers in `search/query.rs:246` / `:335`, scout's same-file scoring, glob filtering on file path, etc. With BGE/E5 (~480 token windows) the threshold is ~50,000-token chunks, but the markdown / image / data-file pipelines can comfortably exceed that, and the `tNwM` form is similarly capped via the inner-digit `bytes.len() >= 4` lower bound only — `t100w0` etc. still parse, but the generic-window arm is the easy regression. The `:t12w99` test is the largest case asserted today.
- **Suggested fix:** Drop the `bytes.len() <= 3` ceiling in the generic `wN` arm — the only structural requirement is `'w'` followed by 1+ ASCII digits to end-of-segment. The current upper bound is purely incidental (it was sized for the v0 100-window cap that no longer exists). Add a regression test for `path:10:abc12345:w100` and `path:10:abc12345:w999`.

#### `+ Inf` note sentiment silently zeroes the boosted chunk's score
- **Difficulty:** easy
- **Location:** src/search/scoring/note_boost.rs:131-134, 204-220 (final `1.0 + s * factor`); pipeline at src/search/scoring/candidate.rs:328
- **Description:** `note_stats` documents that `±Inf` sentiment round-trips through SQLite (`store/notes.rs:492`, asserted by `test_upsert_notes_infinity_sentiment_roundtrips`). Neither `NoteBoostIndex::boost` nor `OwnedNoteBoostIndex::boost` checks finiteness before computing `1.0 + s * note_boost_factor`, so an `+Inf` mention produces a `+Inf` multiplier. In `apply_scoring_pipeline` (`candidate.rs:328`), `base_score.max(0.0) * +Inf == +Inf` (or `NaN` when base is exactly 0.0), the `score >= threshold` guard accepts it, and the candidate flows up to `BoundedScoreHeap::push` (`candidate.rs:213`) where the `is_finite()` check drops it on the floor. Symmetrically, `-Inf` produces `-Inf` and fails `score >= threshold`, returning `None`. Either way, a single mention with extreme sentiment hides every chunk it boosts from search results — exactly opposite to the intent of "boost". (`0.0 * Inf = NaN` is also a separate hit on identical-vector cases.)
- **Suggested fix:** Either (a) clamp sentiment to `[-1.0, 1.0]` (the documented discrete value range from CLAUDE.md) inside `NoteBoostIndex::new` / `OwnedNoteBoostIndex::new`, or (b) reject non-finite sentiments in `notes::upsert_*` so the storage layer enforces the invariant. (a) is a one-line change and matches the existing `clamp(0.0, 1.0)` defense-in-depth pattern in `apply_scoring_pipeline:311`.

#### `compute_scores_opt` per-chunk empty-tokenization fallback emits 0.5 in mixed cohort
- **Difficulty:** medium
- **Location:** src/reranker.rs:299-311
- **Description:** `run_chunk` checks `max_len == 0` and returns `vec![sigmoid(0.0); batch_size]` (= 0.5 per passage) when *this chunk's* longest encoding tokenized empty. The aggregate guard in `compute_scores_opt:267` only short-circuits when the *entire* input is empty. So with a mix of empty + non-empty chunks (e.g. one batch of 32 happens to be all-empty after `take(stage1_limit)` lands on candidates whose passages tokenize to nothing), the empty chunk's 32 results all get score 0.5, and the non-empty chunks return their cross-encoder sigmoid scores in [0, 1]. After `apply_rerank_scores` sorts by score, the 0.5 cohort sits in the middle of the cross-encoder distribution rather than at the tail where missing-data should land. Anything the cross-encoder rated below 0.5 is now ranked behind passages it never saw.
- **Suggested fix:** Return `None` (or a sentinel) for empty chunks and have `compute_scores_opt` fall back to skipping the rerank call rather than producing zero-information scores. If preserving order matters, use the input cosine score (which the candidates already carry as `SearchResult.score`) for empty-tokenization rows so the surviving cohort stays homogeneous.

#### `select_negatives` `take(k)` upstream of empty-content drop yields fewer than k
- **Difficulty:** easy
- **Location:** src/train_data/bm25.rs:155-184
- **Description:** The pipeline is `filter(hash != positive) → filter(content_hash != positive_content_hash) → take(k) → filter_map(drop empty content)`. When some of the top-k entries returned by `score()` map to an empty-content row (rare but possible: pre-content-hash data, or rows where the content was rewritten to empty between BM25 build and select call), the empty rows count toward `k` *before* being dropped, so the caller sees fewer than `k` negatives even when more candidates exist. The docstring promises "top-k negatives".
- **Suggested fix:** Move the empty-content `filter_map` ahead of `take(k)`, or convert the whole chain to a `for` loop that skips empty rows and continues until `k` non-empty negatives are accumulated.

#### `weight as f32` strips NaN guard on sparse vector load
- **Difficulty:** easy
- **Location:** src/store/sparse.rs:381-401
- **Description:** Sparse vector load reads each row's `weight: f64`, applies a `token_id` range check, and pushes `(token_id as u32, weight as f32)` into the per-chunk vector with no finiteness check. SPLADE encoding is non-negative by design (ReLU on logits), but a corrupted row, a future encoder switch, or a manual SQL update can land a `NaN`/`±Inf` weight. Downstream `splade.search_with_filter → IndexResult::score` then carries the bad value into `search/query.rs:543` min-max normalization (`fold(0.0f32, f32::max)` will silently swallow `NaN` because `NaN.max(x) == x` for the `f32::max` total-order convention but `>` returns false on NaN — the normalization branch then divides finite scores by 0.0 and returns 0.0 for everything else). Either path quietly degrades hybrid fusion.
- **Suggested fix:** Filter (with a `tracing::warn!`) or coerce non-finite weights to 0.0 in the load loop, mirroring the `BoundedScoreHeap` is_finite invariant. Same guard belongs at `store/sparse.rs:400` next to the existing token_id range check.

#### CAGRA env-knob defaults accept `0` (parallel to triaged HNSW issue P1-45)
- **Difficulty:** easy
- **Location:** src/cagra.rs:191-203 (`CQS_CAGRA_GRAPH_DEGREE`, `CQS_CAGRA_INTERMEDIATE_GRAPH_DEGREE`)
- **Description:** P1-45 (triaged ✅ #1326) closed the same hole for HNSW `M`/`ef_construction`/`ef_search`, but the CAGRA branch went unmodified: `std::env::var("CQS_CAGRA_GRAPH_DEGREE").ok().and_then(|v| v.parse().ok()).unwrap_or(64)` accepts a literal `"0"` and forwards it to `set_graph_degree(0)`. cuVS treats `graph_degree=0` as "use library default" on some versions and as an error on others, so the user-visible behavior depends on the cuvs pin — exactly the silent-misconfig scenario P1-45 was filed against. `cagra_max_bytes` / `cagra_stream_batch_size` already route through `parse_env_usize` and inherit its `> 0` guard; only these two knobs slipped through.
- **Suggested fix:** Replace the inline `parse().ok().unwrap_or(64)` calls with `crate::limits::parse_env_usize(...)` which already filters non-positive values and logs the rejection.

#### SPLADE min-max normalization collapses everything to 0.0 on negative-only sparse cohort
- **Difficulty:** medium
- **Location:** src/search/query.rs:543-572
- **Description:** `max_sparse = sparse_results.iter().map(|r| r.score).fold(0.0f32, f32::max)`. With a negative-bearing sparse cohort (no real SPLADE input today, but any future sparse signal that is not non-negative — learned dot-product retrievers, contrastive scores, BM25-like deltas) the fold's seed `0.0` dominates, `max_sparse == 0.0`, and the `if max_sparse > 0.0` branch sends every sparse score to `0.0`. The hybrid fuse then degenerates to `alpha * dense + 0.0` — i.e., dense-only retrieval — without any tracing/warning that the sparse leg was suppressed. Even within today's SPLADE-only path, a query whose entire candidate set scores exactly 0.0 (degenerate empty intersection) hits the same branch.
- **Suggested fix:** Initialize the fold from the first element (`iter().map(|r| r.score).reduce(f32::max)`) so the seed can't dominate, and skip normalization (use scores as-is, or warn) when `max_sparse <= 0.0`. Alternatively, log a `tracing::warn!` when `max_sparse == 0.0` so eval/CI catch silent collapse.

#### `chunk.line_end + 1` on `u32` can overflow in `where_to_add` placement suggestion
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:223
- **Description:** `(chunk.name.clone(), chunk.line_end + 1)` adds 1 to a `u32` line number with no saturation. `line_end == u32::MAX` is unreachable for real source files but reachable for fuzzed/corrupted input or the synthetic L5X paths flagged in P1-41. In debug builds this panics; in release it wraps to 0 and silently produces a placement suggestion at line 0, which then breaks any caller relying on `1 ≤ line ≤ file_lines`.
- **Suggested fix:** `chunk.line_end.saturating_add(1)`. Same mechanical fix as the parser's other line-arithmetic guards (`parser/mod.rs:723`, `cache.rs:986`, etc.).

#### `reverse_bfs` (single-source) lacks the stale-queue guard `reverse_bfs_multi` has
- **Difficulty:** medium
- **Location:** src/impact/bfs.rs:50-87 vs 161-215, 229-292
- **Description:** Single-source `reverse_bfs` is correct *today* because BFS visits depths in non-decreasing order and the `!ancestors.contains_key` guard means the first-insertion depth is the minimum. But the moment the function is reused in a loop that pushes back already-seen nodes (e.g. a hypothetical "expand if newer evidence found shorter path" tweak), or merged with `reverse_bfs_multi` for a future caller that wants both single- and multi-source semantics, the missing stale-entry skip block (`if ancestors.get(&current).is_some_and(|&stored| d > stored) { continue; }`) silently regresses to the bug `test_reverse_bfs_multi_stale_queue_entry` was filed against. The two implementations have diverged enough that a reasonable refactor toward "use `reverse_bfs_multi` everywhere" would have to re-discover the property.
- **Suggested fix:** Either (a) reimplement `reverse_bfs(target, depth)` as a thin wrapper over `reverse_bfs_multi(&[target], depth)` so the two share one verified path, or (b) port the stale-entry skip into `reverse_bfs` defensively. (a) eliminates the duplication and the divergence risk in one move.

#### `total_cmp` tie-break on rerank batch fallback puts 0.5 in the middle of cross-encoder cohort
- **Difficulty:** medium
- **Location:** src/reranker.rs:872-908 + 305-311 interaction
- **Description:** Companion to the empty-tokenization finding above, but the algorithm-correctness angle is in `apply_rerank_scores`: the comment justifying the cohort-homogeneous truncation when `scores.len() < results.len()` (`AC-V1.33-9`) is correct, but the symmetric case — `scores.len() == results.len()` with some scores being the 0.5 fallback from `run_chunk` empty-batch — is not addressed. The sort comparator `b.score.total_cmp(&a.score)` then interleaves true cross-encoder scores (in [0, 1]) with synthetic 0.5 fallbacks within the same cohort, and the deterministic id-tiebreak gives them stable but meaningless ranking. Worst case: a true low-relevance score of 0.3 sits below a 0.5 fallback for a passage the encoder never saw.
- **Suggested fix:** Surface the empty-tokenization rows back to `apply_rerank_scores` (e.g., return `Vec<Option<f32>>` from `compute_scores_opt`) and have it either drop those rows (matching the `< n` cohort-trim policy) or fall back to the input cosine score that survived stage 1. Both options keep the comparator on a single homogeneous distribution.


<!-- ===== docs/audit-scratch/batch2-extensibility.md ===== -->
## Extensibility

#### Hardcoded synonym table — no extension hook for domain vocabulary
- **Difficulty:** easy
- **Location:** src/search/synonyms.rs:13-46
- **Description:** `SYNONYMS` is a `LazyLock<HashMap>` baked into the binary with 30 generic developer abbreviations (`auth`, `cfg`, `req`, `db`, etc.). Adding domain vocabulary — industrial automation (`plc`, `scada`, `opc`, `hmi`), manufacturing (`mes`, `erp`, `andon`), or cqs-internal terms (`hnsw`, `splade`, `cagra`, `rrf`, `slot`) — requires editing this file and recompiling. There is no config-side or per-project extension hook. cqs is positioned for manufacturing/industrial use cases per project memory; the synonym dictionary blocks that pivot at the source-edit boundary.
- **Suggested fix:** Load synonyms from a TOML table merged on top of the static defaults: `~/.config/cqs/synonyms.toml` (user-global) and `.cqs/synonyms.toml` (per-project). Each row: `key = ["expansion1", "expansion2"]`. Built-ins remain in source as the floor; project overrides win on conflict. Validation reuses the existing FTS-safe alpha-token check.

#### `apply_parent_boost` hardcodes Class/Struct/Interface as the only container kinds
- **Difficulty:** easy
- **Location:** src/search/scoring/candidate.rs:81-84
- **Description:** Parent-container boost only fires for `ChunkType::Class | Struct | Interface`. cqs already extracts `Trait`, `Enum`, `Module`, `Object`, `Protocol` (and parser/chunk.rs assigns `parent_type_name` for methods on Rust traits, Kotlin objects, Swift protocols, etc.). A query that semantically matches three trait method impls won't boost the trait itself — the heuristic silently degrades to one-result behavior on those languages. Adding a new container variant requires editing this match plus chasing every other `is_container` site.
- **Suggested fix:** Add `ChunkType::is_container() -> bool` on the enum (generated via `define_chunk_types!` macro to keep it data-table-driven), then call `r.chunk.chunk_type.is_container()`. Declare the container flag in the macro row alongside `hints = [...]` and `human = "..."`.

#### `is_test_chunk` heuristic is hardcoded and diverges from SQL-side `TEST_NAME_PATTERNS`
- **Difficulty:** medium
- **Location:** src/lib.rs:502-541, src/store/calls/mod.rs:126
- **Description:** Two parallel test-detection patches: `is_test_chunk` in lib.rs uses tightened name rules (`Test_`, `test_`, `_test`, `_spec`, `.test`) and language-registry path patterns; `TEST_NAME_PATTERNS = &["test_%", "Test%"]` in calls/mod.rs uses looser SQL LIKE patterns that still match `TestSuite` / `TestRegistry` (the production-type case the AC-4 audit explicitly fixed in lib.rs). Adding a new convention — JUnit5 `@DisplayName`-annotated, BDD `_when_should_*`, Go `it_*`, Rust `#[test_case(...)]` — requires touching both sites with different syntaxes (Rust regex vs SQL LIKE). Plus the markers are language-namespaced through `LanguageDef::test_markers` for content but the *name* heuristics are global.
- **Suggested fix:** Move test-name patterns into `LanguageDef::test_name_patterns` (mirroring `test_markers` and `test_path_patterns`), then have both `is_test_chunk` and `TEST_NAME_PATTERNS` consume the registry. Single source of truth, language-scoped, and adding a Kotlin/Swift convention is one line in the language module.

#### `OutputFormat` is a closed enum with hand-coded if/else-if chains at every render site
- **Difficulty:** medium
- **Location:** src/cli/definitions.rs:13-17, src/cli/commands/graph/trace.rs:126-281, src/cli/commands/graph/impact.rs:48-92
- **Description:** `OutputFormat { Text, Json, Mermaid }` is dispatched at 12+ sites via `if matches!(format, OutputFormat::Json) { ... } else if matches!(format, OutputFormat::Mermaid) { ... } else { ... }`. Adding a fourth format (CSV for spreadsheet pipelines, GraphViz/dot for graph commands, Markdown table for PR comments, NDJSON for streaming) requires hunting every render site and adding another `else if`. Worse, the chains aren't exhaustive — the trailing `else` silently falls into Text rendering for unhandled variants, so a new variant will produce text output instead of a compile error.
- **Suggested fix:** Define a `Renderer` trait per command type (`TraceRenderer`, `ImpactRenderer`) with `render_trace(&Trace)` / `render_impact(&Impact)` methods, and a `&[&dyn Renderer]` registry indexed by `OutputFormat`. Replace the if/else-if chain with a single `pick_renderer(format).render(&data)`. Match on the enum at one site (the registry lookup), and let the compiler enforce exhaustiveness via `#[deny(non_exhaustive_omitted_patterns)]`.

#### Hardcoded query classifier function — adding a category requires surgery
- **Difficulty:** medium
- **Location:** src/search/router.rs:632-773 (`classify_query_inner`)
- **Description:** Query classification is an 8-arm hand-coded if/return chain with priority semantics encoded in source order (Negation > Identifier > CrossLanguage > TypeFiltered > Structural > Behavioral > Conceptual > MultiStep). Each arm calls a hardcoded predicate (`is_identifier_query`, `is_cross_language_query`, etc.) and emits a hardcoded `(category, confidence, strategy, type_hints)` tuple. Adding a category — `RegexQuery`, `ApiCall`, `ErrorMessage`, `StackTrace` — requires inserting a new branch at the right priority position, declaring a new predicate, and remembering to add the variant to the `QueryCategory` macro AND to the centroid classifier in `reclassify_with_centroid`. The QueryCategory enum is macro-driven but the *classifier* is not.
- **Suggested fix:** Define `trait QueryClassifier { fn priority(&self) -> i32; fn classify(&self, query: &str, words: &[&str]) -> Option<Classification>; }` and hold a `&[&dyn QueryClassifier]` static slice sorted by descending priority. `classify_query_inner` becomes "first non-None classifier wins, fall back to Unknown." Each existing arm becomes one impl; new categories are one new struct + one slice row.

#### Three near-identical prompt builders — adding a fourth means another method
- **Difficulty:** medium
- **Location:** src/llm/prompts.rs:182-312
- **Description:** `build_contrastive_prompt`, `build_doc_prompt`, `build_hyde_prompt` share the same shape: truncate content, sanitize, fresh sentinel nonce, `format!` with sentinel-bracketed body. The `BatchKind` enum (P3-47 / EX-V1.33-1) abstracted the dispatch but the prompt builders themselves remain three independent methods on `LlmClient`. The doc-comment for `BatchKind` already lists three pending purposes (`Classification`, `ContrastiveRepair`, `CodeReview`) — each will require: (1) a new method on `LlmClient`, (2) a new variant on `BatchKind`, (3) a new arm in the dispatcher mapping kind → builder. Three coordinated edits per new prompt purpose.
- **Suggested fix:** Define `trait PromptBuilder { fn build(&self, item: &BatchSubmitItem) -> String; }` with one impl per kind. Replace `BatchKind` dispatch with a `&[&dyn PromptBuilder]` registry indexed by kind. Adding a fourth purpose: one new struct + one new enum variant + one slice row. The shared sentinel/truncate/sanitize prelude moves into a `BasePrompt` helper that all impls call.

#### `PoolingStrategy` dispatch site loses exhaustiveness via `unreachable!`
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:1390-1399, src/embedder/models.rs:111-137
- **Description:** The 4-variant `PoolingStrategy` is dispatched in the encode loop with `Identity` mapped to `unreachable!()` because it's intercepted earlier as a 2D shortcut. Adding a fifth pooling strategy (e.g., `WeightedMean`, `MaxPool`, `AttentionPool` for LLM2Vec-style heads) means: (1) add variant, (2) implement pooler function, (3) edit dispatch site, (4) decide whether the 2D shortcut applies and edit the 2D path. The `unreachable!` arm is brittle — any future model whose ONNX returns 3D AND uses Identity pooling silently panics in production.
- **Suggested fix:** Make pooling a trait: `trait Pooler { fn pool_3d(&self, hidden: &Array3<f32>, mask: &Array2<i64>, dim: usize) -> Vec<Vec<f32>>; fn handles_2d_directly(&self) -> bool { false } }`. Each variant becomes one impl. The 2D shortcut becomes `if pooler.handles_2d_directly() && hidden.is_2d()`. Removes the unreachable arm and makes adding a new pooler one struct + one enum row.

#### Hardcoded NEGATION / MULTISTEP / cross-language token lists in router
- **Difficulty:** easy
- **Location:** src/search/router.rs (NEGATION_TOKENS, MULTISTEP_PATTERNS_AC, language-name lists in is_cross_language_query)
- **Description:** The classifier predicates rely on a fixed set of English vocabulary: `NEGATION_TOKENS` (without/no/not/...), MULTISTEP conjunctions (then/and then/...), language-name lists (rust, python, ...). Non-English queries, domain-specific negation ("ignoring X"), or new languages added via `lang-*` features don't propagate to the classifier — the language registry is the source of truth for "what languages exist" but the classifier maintains its own parallel list. The `1.95-msrv` edition note in CLAUDE.md mentions "let-chains in if/while are out of scope" — these classifier predicates would benefit from let-chain refactoring once edition bumps, but the underlying vocabulary lock-in is the deeper issue.
- **Suggested fix:** (1) Have `is_cross_language_query` consume `Language::valid_names()` instead of a hardcoded slice — drops one drift point. (2) Move NEGATION_TOKENS / MULTISTEP_PATTERNS to `~/.config/cqs/classifier.toml` with built-in defaults, mirroring the synonym fix above.

#### LLM model-name validation hand-coded per provider
- **Difficulty:** medium
- **Location:** src/llm/mod.rs:326-326 + each `ProviderRegistry::build` impl
- **Description:** While the `ProviderRegistry` trait is properly pluggable (P4-EX work), each provider's model-name validation is implicit / informal — `LocalProvider::new` parses `LlmConfig.model` as a string with no constraint, so `cqs feedback --provider anthropic --model gpt-4o` silently submits to Anthropic with an OpenAI model name and fails at API time with a vague error. There's no `provider.is_valid_model(name) -> Result<(), Error>` step before submission.
- **Suggested fix:** Add `fn validate_model(&self, name: &str) -> Result<(), LlmError>` to `BatchProvider` (or `ProviderRegistry`). Anthropic checks against a known prefix list (`claude-`, `claude-3`, etc.); Local accepts anything because OpenAI-compat servers expose arbitrary model names; a future OpenAI provider can validate `gpt-` / `o1-` / `o3-`. Call it from `submit_batch` before the API roundtrip. Adds one method per provider but catches the fast-fail case.

#### CAGRA threshold and backend selection are env-var-driven, not policy-extensible
- **Difficulty:** medium
- **Location:** src/index.rs (BackendContext) + cagra/hnsw backends + `CQS_CAGRA_THRESHOLD` env var
- **Description:** The `IndexBackend` registry is now table-driven (P4-EX work) but the *selection policy* — "use CAGRA when chunk_count >= 5000 and GPU available" — is hand-coded inside each backend's `try_open`. There's no shared `SelectionPolicy` trait, so adding a third backend (USearch, Metal, ROCm, SIMD brute-force) requires re-deriving "when should I be picked" from ad-hoc env vars and chunk-count thresholds. The `i32 priority()` is iteration order, not a real eligibility rule. This blocks slot-aware policies like "prefer USearch on slot=foo because it's tuned for that corpus shape" without a config-side knob.
- **Suggested fix:** Promote the eligibility logic to a `SelectionPolicy` shape on `BackendContext` (e.g., `cqs.toml [index.policy]` with `prefer = "cagra"`, `cagra_min_chunks = 5000`, `disabled_backends = ["usearch"]`), then have each backend's `try_open` read it from `ctx.policy`. New backends declare their default policy entry; operators override per-slot in TOML.


<!-- ===== docs/audit-scratch/batch2-platform.md ===== -->
## Platform Behavior

#### `apply_resolved_edits` flattens CRLF source files to LF on every doc rewrite
- **Difficulty:** medium
- **Location:** src/doc_writer/rewriter.rs:478-512 (`apply_resolved_edits`)
- **Description:** The doc-rewrite path reads the file with `read_to_string`, then `apply_resolved_edits` does `content.lines().map(|l| format!("{l}\n"))`. `str::lines()` strips both `\r\n` and `\n`; the re-emit is bare `\n`. Result: every `cqs index --improve-docs` (or any `rewrite_file` caller) silently rewrites a Windows-source file from CRLF to bare LF across the whole file, churning every line in git's diff and fighting `core.autocrlf=true`. Same root cause that `note.rs::write_notes_file` already fixed (PB-V1.33-9 / #1356) — that fix sniffed CRLF in the existing content and re-translated `\n → \r\n` before writing. The doc rewriter has no equivalent guard. `compute_rewrite_from_content` (line 263) feeds `apply_resolved_edits` directly, and `write_proposed_patch` (line 523) builds `similar::TextDiff::from_lines` over the lossy `new_content`, so even the patch-only path emits a diff that flips line endings on every line.
- **Suggested fix:** Mirror the note.rs CRLF-preservation pattern: sniff `content.contains("\r\n")` once, switch the re-join to `"\r\n"` if so. Or — more robustly — split using a regex / byte-level scan that preserves the per-line trailing terminator instead of `str::lines()`. The note.rs comment block at line 349-373 is the template.

#### `audit-mode.json` SEC-1 promise is bypassed on Windows
- **Difficulty:** medium
- **Location:** src/audit.rs:140-159 (`save_audit_state`)
- **Description:** P4-19 in the v1.33 triage. Confirmed still present at v1.36.2: the `#[cfg(unix)]` arm uses `OpenOptions::mode(0o600)` so the tmp file is born private; the `#[cfg(not(unix))]` arm calls `std::fs::write(&tmp_path, &content)` with no ACL hardening at all, then `atomic_replace` moves it into `.cqs/audit-mode.json` with whatever default ACL the OS picks. The same shape exists in `project.rs::save` (line 129-132), `config.rs:899-905` and `config.rs:997-1003`. Audit mode encodes "the operator believes notes are untrusted" — leaking that state to other Windows users on a multi-user host (or to per-user services like the embedded SYSTEM-runtime indexer) breaks SEC-1's "0o600-equivalent" claim that doc-comments and SECURITY.md make.
- **Suggested fix:** Wire a Windows ACL fixup (e.g., `windows-acl` or a manual `SetFileSecurityW` call to deny everyone-except-owner) inside the `#[cfg(not(unix))]` arms. Alternatively, downgrade SEC-1's claim in SECURITY.md so the doc and the code agree (per the "docs lying is P1" rule).

#### `db_file_identity` Windows fallback uses mtime — misses rapid `--force` rebuilds
- **Difficulty:** medium
- **Location:** src/cli/watch/reindex.rs:42-45
- **Description:** P4-17 in the v1.33 triage, still present. On Unix the daemon detects DB-replacement via `(dev, ino)` after `cqs index --force`; on Windows the fallback returns `metadata.modified()`. NTFS mtime granularity is 100ns in theory but Win32 file caching often returns ~1-second-old timestamps, and a sub-second `index --force` (very common in tests / quick re-indexes) leaves the daemon serving the stale handle because `db_id == old_db_id`. The downstream code at `cli/watch/mod.rs:1281,1333,1461,1506` re-reads the index in lockstep with this signal, so a stale `db_file_identity` on Windows means the daemon hands out queries against a freed mmap — at best wrong results, at worst a SIGBUS-equivalent if NTFS truncates.
- **Suggested fix:** On Windows use `meta.file_index()` (nightly) or call `GetFileInformationByHandleEx` for the file index + volume serial number — that pair is the NTFS analogue of `(dev, ino)`. The `winapi`/`windows-sys` crate already pulls in the necessary bindings.

#### `worktree::lookup_main_cqs_dir` doesn't canonicalize `dir`, so worktree-vs-main equality is asymmetric on case-insensitive filesystems
- **Difficulty:** medium
- **Location:** src/worktree.rs:119-140
- **Description:** `lookup_main_cqs_dir(dir)` joins `dir` with `INDEX_DIR` directly (line 120) and then calls `resolve_main_project_dir(dir)` (line 124), which `dunce::canonicalize`s only the resolved `.git/` parent. The returned `MainIndexLookup::WorktreeUseMain { worktree_root: dir.to_path_buf(), main_root, .. }` therefore has a canonicalized `main_root` but a non-canonical `worktree_root`. Downstream callers compare these fields (search-result `_meta.worktree_stale`, daemon ping responses) and string-compare against `find_project_root()` output, which IS canonicalized via `dunce::canonicalize` (cli/config.rs:138). On Windows `C:\Projects\Foo` vs `C:\projects\foo` and on macOS `/Users/Foo` vs `/Users/foo` produce a worktree_root that doesn't equal find_project_root's output, so the `worktree_stale` flag fires (or fails to fire) inconsistently — same lab as the #1254 leakage class.
- **Suggested fix:** Call `dunce::canonicalize(dir)` once at the top of `lookup_main_cqs_dir` and use that for both the `own_cqs.exists()` check and `MainIndexLookup::*::worktree_root`. Same for `MainIndexLookup::WorktreeMainEmpty.worktree_root`.

#### Multiple `strip_prefix(root)` callers silently leak absolute paths on case-skew without the warn shim
- **Difficulty:** medium
- **Location:** src/scout.rs:319, src/gather.rs:269, src/onboard.rs:160 + 413, src/doc_writer/rewriter.rs:543
- **Description:** `enumerate_files` learned (lib.rs:943) that on case-insensitive FS, `path.starts_with(root)` can pass while `path.strip_prefix(root)` byte-equals fails — the fix logs a warn and skips. Five other call sites still use the original anti-pattern: `file.strip_prefix(root).unwrap_or(&file).to_path_buf()`. When the user's `find_project_root` canonicalization picks `C:\Projects\Cqs` but the SQLite-stored chunk paths were ingested as `C:\projects\cqs`, every JSON envelope leaks the full absolute path into a field documented as "relative to project root", and downstream agents that string-compare `file` against `<root>/...` mismatch silently. No warn fires — the operator only sees "scout returned full paths" without explanation. Equivalent on macOS HFS+ default and on any Linux ZFS pool with `casesensitivity=insensitive`.
- **Suggested fix:** Wrap the pattern in a single helper `relativize_or_warn(file, root)` that returns `Option<PathBuf>` with the same shim that `enumerate_files` already emits, and switch all five call sites to it. Or normalise both sides to lowercase comparison on `cfg(any(target_os = "windows", target_os = "macos"))` before stripping.

#### `note::path_matches_mention` is case-sensitive, breaks on Windows / macOS notes
- **Difficulty:** easy
- **Location:** src/note.rs:525-542
- **Description:** Note mentions are matched via `str::strip_prefix` / `strip_suffix` after slash-normalization. Case-sensitive byte comparison. On Windows a note mentioning `"Cargo.toml"` won't match an indexed chunk file `"cargo.toml"` (and vice versa for path components like `Src/` vs `src/`). The CLAUDE.md and README explicitly invite users to commit `docs/notes.toml` cross-platform, so a note authored on Linux silently fails to apply on Windows. Worse, the sentiment-boost in the search ranker therefore vanishes for affected files — silent quality regression rather than a hard error.
- **Suggested fix:** On `cfg(any(target_os = "windows", target_os = "macos"))`, use `eq_ignore_ascii_case` for the strip checks (or normalise both strings to lowercase before comparison). On Linux keep the case-sensitive behaviour — Linux ext4 is case-sensitive and `Cargo.toml`-vs-`cargo.toml` collisions there are intentional.

#### `daemon_translate` is unconditionally `#[cfg(unix)]` — `cqs ping`, `cqs watch --serve`, daemon RPC all silently degrade on Windows
- **Difficulty:** hard
- **Location:** src/daemon_translate.rs (29× `#[cfg(unix)]`, zero `#[cfg(not(unix))]` shims), src/cli/commands/infra/ping.rs:166-200
- **Description:** Every public daemon-RPC entry point — `daemon_socket_path`, `daemon_ping`, `daemon_status`, `daemon_reconcile` — is gated `#[cfg(unix)]` only. Windows builds ship `cqs ping` that prints "not supported, exit 1" (per ping.rs:160-166) and `cqs watch --serve` falls back to CLI-only mode. README.md's release matrix advertises `Linux x86_64, macOS ARM64, Windows x86_64` and the CLAUDE.md / docs talk about the daemon's 3-19ms-vs-2s startup advantage as a blanket claim. Windows users get the cold-start path on every invocation with no warning at install time. Either the docs lie, or Windows needs a `Named Pipe` daemon path. Docs-lying-is-P1 (per memory.md).
- **Suggested fix:** Two options. (1) Implement a `\\.\pipe\cqs-<hash>` named-pipe variant for Windows — the wire format is already JSON-line so the transport swap is mostly mechanical. (2) Update README + SECURITY.md to explicitly mark the daemon as Linux+macOS-only and remove Windows from the daemon-aware perf claims. Either way, `cqs ping` on Windows should print a one-line "daemon not implemented on Windows; using CLI mode (~2s startup)" hint instead of failing with exit 1.

#### `tasklist` parsing assumes UTF-8 stdout — Windows console code pages can corrupt the comparison
- **Difficulty:** medium
- **Location:** src/cli/files.rs:104-111
- **Description:** `String::from_utf8_lossy(&o.stdout)` on `tasklist /FO CSV /NH` output. The `/FO CSV` format is locale-independent for the *separator* characters (comma, double-quote, PID digits — all ASCII), so the substring match `,\"<pid>\",` actually works correctly even when CMD's active code page is CP932 (Japanese) or CP1252 (Windows-1252) — those are ASCII-supersets. BUT: tasklist on some Windows builds emits a UTF-16 LE BOM (`\xFF\xFE`) when invoked with `/FO CSV` from certain process contexts (notably PowerShell-spawned children with `[Console]::OutputEncoding = Unicode`). `from_utf8_lossy` then sees `\xFF\xFE,...` and the BOM-prefixed PID column never matches `,"12345",`. `process_exists` returns false, which `acquire_index_lock` interprets as "stale lock holder" and races the live daemon for the lock file — the exact bug PB-V1.33-10 was supposed to fix.
- **Suggested fix:** Detect a UTF-16 LE BOM in the first two bytes of stdout and decode via `String::from_utf16_lossy(...)` before the substring search. Or pass `/FO LIST` and parse line-by-line — LIST format prefixes each field with the field name and is identically-shaped across encodings.

#### Hardcoded `/tmp` in `Path::new` test fixtures fails on Windows (test-portability bug, not runtime)
- **Difficulty:** easy
- **Location:** src/store/notes.rs:331,352,419,435,470,499,520,546; src/store/backup.rs:373-399; src/cli/watch/tests.rs:164-300+
- **Description:** Many test cases construct `Path::new("/tmp/notes.toml")` or `PathBuf::from("/tmp/test_project")` as a placeholder label. They work on Linux/macOS (the path is just stored as a string, never opened), but on Windows `Path::new("/tmp/...")` is a relative path under whichever volume the test runner is on, so behaviour diverges from intent. The `_test_project` watch tests at cli/watch/tests.rs make assumptions (e.g., that `/tmp/test_project/.cqs/index.db` is "inside" `/tmp/test_project`) that hold on POSIX but produce different `starts_with` results on Windows. Not load-bearing today because the project's CI doesn't run the full Windows test matrix, but it bites whoever stands up Windows CI next.
- **Suggested fix:** Replace `/tmp/<x>` with `tempfile::TempDir::new().unwrap().path().join("<x>")` for any test that exercises path-comparison logic; for tests that only need a label string (notes.toml), use `Path::new(if cfg!(windows) { "C:\\test\\notes.toml" } else { "/tmp/notes.toml" })` or just `Path::new("notes.toml")` if absoluteness doesn't matter.

#### `lookup_main_cqs_dir` predicates on `own_cqs.exists()` without distinguishing dir-vs-file
- **Difficulty:** easy
- **Location:** src/worktree.rs:120-122
- **Description:** `if own_cqs.exists()` — on a worktree where someone created a `.cqs` regular file (e.g. `touch .cqs` by mistake, or a packaged tarball with a stray entry) the code returns `MainIndexLookup::OwnIndex { path: own_cqs }` and downstream code tries to open `<file>/index.db` which fails with a confusing "is not a directory" error rather than the clean "this worktree has no index, use main's" error path. Cross-platform issue but most likely on Windows where users hand-edit zip-extracted directories.
- **Suggested fix:** Use `own_cqs.is_dir()` rather than `own_cqs.exists()`. Same fix in the `main_cqs.exists()` branch at line 128.


<!-- ===== docs/audit-scratch/batch2-security.md ===== -->
## Security

#### Store::open creates index.db with permissive umask, post-open chmod has TOCTOU window
- **Difficulty:** easy
- **Location:** src/store/mod.rs:973-1014
- **Description:** `connect_with_config` lets sqlx create `index.db`, `db-wal`, and `db-shm` honoring the user's umask (typically 0o022 → 0o644 / world-readable), then post-fix permissions to 0o600 only after `connect_with(...).await`. On a multi-user box, any local user can read the freshly-born WAL/SHM during the window between SQLite's first commit and the `set_permissions` call. The cache path (`src/cache.rs:295-420`) closed exactly this gap with SEC-V1.33-2 by wrapping pool creation in `libc::umask(0o077)` + restore. The main store, which holds the most sensitive data (chunks, embeddings, summaries, notes derivatives), still has the race. Read-only opens skip the chmod entirely, so a concurrent process opening read-only never tightens perms it inherited from the original creator.
- **Suggested fix:** Mirror the cache pattern: tighten umask to 0o077 around `pool.connect_with(connect_opts)` for write opens, then restore. Keep the post-open `set_permissions` as belt-and-suspenders (matches the cache comment).

#### Daemon `tracing::info!` snapshot logs every CQS_* env var to journald
- **Difficulty:** easy
- **Location:** src/cli/watch/mod.rs:633-656
- **Description:** Daemon startup logs every `CQS_*` env var at info level into journald with a hardcoded suffix-only redactor (`_API_KEY`, `_TOKEN`, `_PASSWORD`, `_SECRET`). Misses common shapes used in this codebase: `ANTHROPIC_API_KEY` (no `CQS_` prefix is fine — it's filtered by prefix anyway, so safe), but `CQS_LLM_API_BASE` carrying `https://user:pass@host/...`, `CQS_PROXY_URL`, `CQS_AUTH_TOKEN` style names not ending in one of the four exact suffixes, or any user-extension env (`CQS_FOO_BEARER`, `CQS_VLLM_KEY`) — none redacted. journald keeps this for 30 days per the rotation policy mentioned in `auth.rs:151`. With OB-V1.30-1 surfacing info-level to journald, operators harvesting logs find these.
- **Suggested fix:** Switch the predicate from "name ends with one of 4 suffixes" to "name contains any of {KEY, TOKEN, SECRET, PASSWORD, BEARER, AUTH, CRED, PASS}" (case-insensitive substring), or take a deny-by-default approach for any name not on a small allowlist (`CQS_LLM_MODEL`, `CQS_LLM_API_BASE` minus userinfo, etc.).

#### `open_browser` on Windows passes URL with auth token to `cmd /C start "" <url>` — cmd.exe metachar parsing
- **Difficulty:** medium
- **Location:** src/cli/commands/serve.rs:127-156
- **Description:** `cmd /C start "" "<url>"` and the WSL `cmd.exe /C start "" "<url>"` branches hand the listening URL (which contains `?token=<43char>`) to `cmd.exe`. `Command::new("cmd").arg(url)` quotes the arg for CreateProcess but `cmd.exe` then re-parses, treating `&`, `^`, `|`, `>`, `<`, `(`, `)`, `%` as shell metacharacters even inside double quotes for pipe and redirect operators when they appear unquoted post-expansion. The current bind-addr + token form (`http://127.0.0.1:8080/?token=ABC`) is alphanumeric so today is safe, but: (a) any future addition to the URL (e.g. `?token=X&foo=bar`) would be split by cmd.exe into two commands; (b) `--bind` accepts arbitrary user input, so a hostile config file or shell script that sets `bind = "127.0.0.1:8080/&calc&"` — the token alphabet is enforced, but the URL host:port comes from user input via `bind_addr`. This is a latent foot-gun more than an active exploit.
- **Suggested fix:** Use `cmd.exe /D /S /C` and pass URL via `start` with a verified-safe URL; or skip `cmd.exe` and call `ShellExecuteW` via the `windows` crate / `open` crate directly so cmd.exe's parser is bypassed.

#### LLM error path logs full HTTP body at `tracing::debug!` — prompts + reflected secrets land in journald
- **Difficulty:** easy
- **Location:** src/llm/batch.rs:87-94 (and 131-137, 234-240)
- **Description:** On any non-success response from the Anthropic batch endpoint, `tracing::debug!(body = %body, ...)` logs the entire HTTP error body at debug level. Anthropic's 4xx responses regularly echo input fields (the prompt, the offending content). Operators who run `RUST_LOG=cqs=debug` (recommended in CONTRIBUTING.md) get the full prompt — which on cqs's pipeline is *the indexed source code, signatures, and surrounding context* — into journald. If the input body contained anything sensitive (private repo content, secrets accidentally indexed via `extra_paths`), they're now in the 30-day journal. The user-visible `LlmError::Api { message }` correctly uses `redacted_api_message`; the debug log undoes that.
- **Suggested fix:** Drop `body = %body` from the debug log entirely. Keep `body_len` and `parsed_anthropic_message` (if you trust Anthropic's `error.message` not to echo prompts — it does sometimes, so consider dropping that too). For local-debugging, gate the full body behind `tracing::trace!`.

#### `slot_dir` / `slot_config_path` build paths from unvalidated slot name (read path)
- **Difficulty:** easy
- **Location:** src/slot/mod.rs:194-244 (consumers in src/lib.rs:377, src/cli/store.rs:49, src/cli/watch/mod.rs:520, etc.)
- **Description:** `slot_dir(project_cqs_dir, slot_name)` and `slot_config_path` do a raw `project_cqs_dir.join(SLOTS_DIR).join(slot_name)` with no `validate_slot_name` call. The write paths (`write_slot_model`, `slot_create`, etc.) all validate up front. The read paths — `read_slot_model:257`, the public `cqs::resolve_slot_dir:377` (lib export), and every `slot_dir(...)` call site that takes a slot name from `CQS_SLOT` env, `--slot` CLI flag, or `.cqs/active_slot` pointer — do not. `resolve_slot_name` validates names that come through *its* code path, but `slot_dir` is a public helper and external code (or future call sites) can hand it `"../../etc/passwd"`, getting a `<project>/.cqs/slots/../../etc/passwd` path. `Path::join` does not normalize `..`. Today the consumers pass the path to SQLite, which would just fail noisily — not a live exploit — but a future caller that uses the path for `read_dir` / file enumeration will leak.
- **Suggested fix:** Make `slot_dir` and `slot_config_path` either return `Result<PathBuf, SlotError>` and call `validate_slot_name` internally, or accept a typed `&ValidatedSlotName` newtype that can only be constructed via `validate_slot_name`. The current shape invites the bug.

#### `cqs serve` — full request URI not redacted in axum extractor errors
- **Difficulty:** medium
- **Location:** src/serve/handlers.rs:147-159, 244-270
- **Description:** P1.11 (already fixed) made the TraceLayer span record `path` only, not the full URI, so `?token=…` doesn't bleed into structured logs. But when axum's built-in `Query<T>` extractor fails (malformed query string, type-mismatched arg), axum's default rejection emits the *full URI fragment* in its 400 response body and emits a debug log via `tower_http`. The `cqs serve` router includes a `TraceLayer::new_for_http()` that captures `make_span_with` for the *successful* path, but extractor failures fall through to the layer's default error response handler which logs the unparsed query. A client sending `GET /api/graph?token=ABC&max_nodes=notanumber` produces a 400 whose body and the trace event both include `token=ABC`. The auth middleware sits *inside* the extractor for query handlers (it's added via `from_fn_with_state` in `build_router:320` after the routes), so an attacker can't reach the handler — but the 401 path in `enforce_auth:657-662` already strips query strings. The extractor-failure path doesn't.
- **Suggested fix:** Add a custom `axum::error_handling` layer that intercepts 400-class extractor rejections and rewrites the response body / log fields to drop any `token` parameter. Or: parse query strings manually in handlers (verbose) so the extractor's error path can't fire on auth-bearing requests.

#### `validate_repo_id` allows `..` in HF model name → optimum subprocess fetch
- **Difficulty:** easy
- **Location:** src/cli/commands/train/export_model.rs:17-30
- **Description:** The repo-id allowlist permits `[A-Za-z0-9._/-]` which lets `..` through (`.` in the set + `.` in the set = `..`). A repo id like `org/../../etc` or `../foo` is accepted and passed verbatim to `optimum.exporters.onnx --model <repo>`. Optimum first tries to interpret `--model` as a local path before falling through to HF Hub. A repo id `../../home/user/secrets` would cause optimum to attempt loading from `<cwd>/../../home/user/secrets` as a local model directory, surfacing the existence (and possibly partial content via error messages) of that path in the user-visible failure. Low impact today — failure surface only — but the in-source comment on line 14 says "HuggingFace repo IDs are documented as `[A-Za-z0-9._/-]` only" and HF Hub explicitly rejects `..` for this reason; cqs is laxer than the upstream contract.
- **Suggested fix:** Add `if repo.contains("..") { bail!(..) }` to `validate_repo_id`, mirroring the `embedder/models.rs:698` SEC-28 check on the user-config repo path which already excludes `..`.

#### Audit-mode TOCTOU on Windows — no ACL or atomic create
- **Difficulty:** medium
- **Location:** src/audit.rs:140-160
- **Description:** Acknowledged at audit-triage P4-19 as a known issue. Recording sub-aspect: even on Unix, `OpenOptions::new().create(true).truncate(true).open(&tmp_path)` creates the temp file with the user's umask if the `.mode(0o600)` builder method is somehow bypassed by a future refactor — `.mode()` is on `OpenOptionsExt`, not on the cross-platform `OpenOptions`, so a clippy lint or a Windows-clean refactor could silently drop it. Hold-out from #1107 / #1108 era: the same pattern lives in `notes.rs`, `project.rs`, `slot/mod.rs`, `audit.rs` — five separate atomic-write paths with the same Unix-only chmod, three of which (`audit`, `project`, `slot`) overlap. None has a Windows equivalent.
- **Suggested fix:** Centralize the "private temp file" pattern in `crate::fs` (next to `atomic_replace`) so a single `private_tempfile_for(path)` helper sets `.mode(0o600)` on Unix and applies a sensible Windows ACL (deny `Authenticated Users : Read`) via `windows::Win32::Security`. All five callers reduce to one line and Windows finally gets parity.

#### `serve` request-body limit (64 KiB) sits outside the auth layer — pre-auth memory pressure
- **Difficulty:** easy
- **Location:** src/serve/mod.rs:341-344
- **Description:** Comment says the body limit "applies even to rejected requests (preventing OOM-then-401 attacks)" — it does, but only because axum applies layers in the documented order. A 64 KiB cap means an attacker on the host-allowlist (or via a WebSocket-upgrade attempt) can fan out N concurrent connections, each holding a 64 KiB body buffer pre-auth. With the default tokio worker pool, the pre-auth body buffers can cumulatively allocate ~64 KiB × N where N is bounded only by the OS file descriptor limit, since the `blocking_permits` semaphore (P2.76) gates only the blocking pool, not the async accept loop. For a localhost-bound box this is academic; for the `--bind 0.0.0.0` path with `--no-auth` documented carve-out, less so.
- **Suggested fix:** Cap concurrent inbound connections with `tokio::sync::Semaphore` outside the body-limit layer (per-IP would be ideal but requires extracting the peer addr). Or document explicitly that operators must use a reverse proxy in front for the LAN-bind path.

#### Daemon socket cleanup races a hostile local user — bind succeeds with TOCTOU
- **Difficulty:** medium
- **Location:** src/cli/watch/mod.rs:540-611
- **Description:** Cleanup logic uses `symlink_metadata` + `remove_file` + `bind`. Between the `remove_file` and `UnixListener::bind`, a local user can `socket(AF_UNIX) + bind(sock_path)`, planting their own socket — the next `bind()` call from the daemon then either fails (and the daemon exits) or, on platforms where `bind()` follows symlinks (Linux does not, BSD differs), connects through. The umask 0o077 wrap closes the *permission* race but not the *identity* race. If the daemon socket lives under `/tmp/cqs-<hex>.sock` (the `XDG_RUNTIME_DIR` unset fallback at `daemon_translate.rs:228`), `/tmp` is world-writable so the race window matters. The hex is `blake3(canonical_project_path)`; an attacker who knows the project root (often visible in the systemd unit file or `ps`) can pre-compute it and squat on the socket name during early daemon boot.
- **Suggested fix:** Bind under `XDG_RUNTIME_DIR` whenever it's set (already done) and refuse to start when it isn't — or `mkdir -m 0700 /tmp/cqs-<uid>/` first and bind inside that, so the parent dir's perms gate squatters even when `/tmp` itself is sticky-but-shared. Document the `XDG_RUNTIME_DIR`-required posture in SECURITY.md.


<!-- ===== docs/audit-scratch/batch2-data-safety.md ===== -->
## Data Safety

#### DS-V1.36-1: `collect_migration_files` misses `hnsw.ids` / `hnsw.checksum` / `index_base.*` sidecars
- **Difficulty:** easy
- **Location:** src/slot/mod.rs:912-927 (`collect_migration_files`)
- **Description:** The hardcoded `candidates` list moved by `migrate_legacy_index_to_default_slot` includes only `index.hnsw.data` and `index.hnsw.graph` (and the same two for `index_base`). It is missing `index.hnsw.ids`, `index.hnsw.checksum`, `index_base.hnsw.ids`, `index_base.hnsw.checksum`, `index.hnsw.lock` for `index_base`, and SPLADE's checksum-bearing companion files. Post-DS-V1.33-3 (PR #1325), `verify_hnsw_checksums` is strict: a checksum file referencing files that do not exist is a hard error. A user with a v1.32-era pre-slot legacy `.cqs/` containing all five `index.hnsw.*` files would, after `cqs slot ...` triggers the legacy migration, end up with `slots/default/index.hnsw.{data,graph}` only — `index.hnsw.ids` and `index.hnsw.checksum` are left behind in `.cqs/`. The next `cqs search` then either fails verification (no `hnsw.ids` to load) or rebuilds from scratch silently (because the `Self::load_with_dim` "any required file missing" path returns `NotFound`), losing all enrichment work captured in the original index. Also worth noting: `HNSW_ALL_EXTENSIONS` in `src/hnsw/persist.rs:101-108` is the canonical list — `collect_migration_files` should iterate that constant + `index_base` + SPLADE rather than hard-coding a partial list.
- **Suggested fix:** Replace the hardcoded `candidates` array with a programmatic enumeration: for each basename in `["index", "index_base"]` and each ext in `crate::hnsw::persist::HNSW_ALL_EXTENSIONS`, build `<basename>.<ext>`. Then add splade companion files. Add a `cargo test` regression that drops a fake legacy `.cqs/` containing all five `index.hnsw.*` files plus `splade.index.bin`, runs the migration, and asserts every file is present in `slots/default/`.

#### DS-V1.36-2: CAGRA `save` overwrites the previous good blob in place — no `.bak` or temp-rename
- **Difficulty:** medium
- **Location:** src/cagra.rs:888-975 (`CagraIndex::save`)
- **Description:** `gpu.index.serialize(&gpu.resources, path_str, true)` writes the cuVS blob directly to the final path. There is no temp-file staging or `.bak` rollback. A crash, OOM, or SIGKILL during `cuvsCagraSerialize` truncates or corrupts the only on-disk copy of the previous good `.cagra` index — and because the meta sidecar was already removed at line 924 (`let _ = std::fs::remove_file(&meta_path);`), the old metadata that would let load() validate the prior good blob is also gone. Net effect: a single ill-timed crash during `cqs index` finalization leaves the user with a partial `.cagra` and no `.meta`, which load() will reject as `NotFound` — forcing a full GPU rebuild (~minutes for a 50k-chunk corpus, much worse on the inference RTX 4000). The HNSW save path uses backup-temp-rename rollback (DS-V1.30.1-D7); CAGRA does not. The save is best-effort by design (load can rebuild) but the destructive overwrite is avoidable.
- **Suggested fix:** Stage cuVS output to `<path>.{:016x}.tmp` (using `crate::temp_suffix()` for the same-process collision protection used elsewhere), checksum it, then `crate::fs::atomic_replace(&tmp, path)`. Mirror the `.bak` rotation pattern used in `src/hnsw/persist.rs:467-484` so a save failure can restore the prior blob. Remove the meta_path *after* the new blob is in place, not before.

#### DS-V1.36-3: legacy `hnsw_dirty` key is never cleared after per-kind keys are written
- **Difficulty:** easy
- **Location:** src/store/metadata.rs:257-278 (`is_hnsw_dirty`) and 437-451 (`set_hnsw_dirty`)
- **Description:** The doc comment on `is_hnsw_dirty` claims that "the next `set_hnsw_dirty` call splits them apart" — meaning the legacy single `hnsw_dirty` key should be removed once a per-kind key is written. `set_hnsw_dirty` does not implement that: it only writes `hnsw_dirty_enriched` or `hnsw_dirty_base`, never deleting the legacy key. So on a v1.20-era DB with `hnsw_dirty=1` still in metadata, after `set_hnsw_dirty(Enriched, false)`, querying `is_hnsw_dirty(Base)` still falls through to the legacy key and returns `true` — Base is permanently reported dirty even though the user never wrote anything to it. The inverse case (legacy=0, per-kind enriched=1) makes `is_hnsw_dirty(Base)` return false even if Base was secretly dirty, which is the actually-dangerous direction: Base could go un-rebuilt indefinitely.
- **Suggested fix:** In `set_hnsw_dirty`, after writing the per-kind key, run `DELETE FROM metadata WHERE key = 'hnsw_dirty'` inside the same transaction. The legacy fallback in `is_hnsw_dirty` then becomes dead code that can be removed in a subsequent cleanup, but for safety keep it for one release while real DBs migrate.

#### DS-V1.36-4: `cqs index --force` deletes WAL/SHM without checkpointing the old store first
- **Difficulty:** easy
- **Location:** src/cli/commands/index/build.rs:362-386 (`--force` rebuild path)
- **Description:** The summary-recovery branch runs `Store::open` to read summaries, drops the store (line 362), then renames `index.db` → `index.db.bak` and unconditionally `remove_file`s `index.db-wal` and `index.db-shm`. The drop path on `Store` runs only `wal_checkpoint(PASSIVE)` with a 1s timeout (V1.36.2 / src/store/mod.rs:1430-1480); PASSIVE bails on any reader/writer contention without copying. So if the user happens to have a `cqs watch` daemon attached to the same DB (legitimate workflow — watch is supposed to coexist with index), the PASSIVE checkpoint can return without flushing pages. Then deleting `-wal` discards any uncommitted-from-mainfile pages permanently. The summaries we just read are unaffected (they came from a fresh `SELECT`), but any other writes the daemon was making (e.g. notes, telemetry counters) get silently truncated. The previous `Store::close()` API (which runs `TRUNCATE`) is not invoked here.
- **Suggested fix:** Replace `drop(old_store)` (line 362) with `old_store.close().context("...")?;` so the `wal_checkpoint(TRUNCATE)` runs synchronously. Then the `-wal`/`-shm` removal at lines 376-385 is safe because all pages have been merged into `index.db` before the rename. This is the structured-shutdown path the V1.36.2 fix preserved precisely for this kind of operator-initiated quiesce.

#### DS-V1.36-5: SPLADE save uses two-pass rewrite without `.bak` rollback for the final file
- **Difficulty:** medium
- **Location:** src/splade/index.rs:350-440 (`save`)
- **Description:** The SPLADE save streams a blake3 hash through a `HashingWriter`, then seeks back to the checksum offset to stamp the digest, then `atomic_replace`s the temp into place. That part is sound. But unlike the HNSW save (which renames the existing `<basename>.<ext>` to `.<ext>.bak` *before* atomic_replace runs and rolls back on failure), SPLADE's atomic_replace overwrites the previous good `splade.index.bin` directly. If `atomic_replace` itself fails (e.g. cross-device fallback runs out of disk after the source file was already promoted but before the rename completes), the previous good index is gone and the temp may also be corrupt. The `splade_generation` counter then diverges from the on-disk file: SPLADE refuses to load (generation mismatch), so we fall back to no-SPLADE search. Recovery requires a full reindex. The HNSW pattern (rename to `.bak` first, restore from `.bak` on rollback) is precisely the missing piece.
- **Suggested fix:** Before `atomic_replace`, rename any existing `splade.index.bin` to `splade.index.bin.bak`. On atomic_replace failure, rename `.bak` back to the live name. On success, remove `.bak` after a parent fsync. Mirrors `src/hnsw/persist.rs:467-484`.

#### DS-V1.36-6: HNSW load `_lock_file` drop happens at end of function — but lock is taken without timeout, can hang load on stuck save
- **Difficulty:** medium
- **Location:** src/hnsw/persist.rs:736-758 (lock acquisition in `load_with_dim`)
- **Description:** The shared lock taken on `<basename>.hnsw.lock` during load is acquired via `lock_file.lock()` (the std file-lock API) with no timeout or `try_lock` fallback. If a concurrent `save()` from a different process holds the exclusive lock and that process is wedged (e.g. WSL 9P stall, paused under debugger), the load blocks indefinitely. `cqs search` then hangs and the user has no signal except "command never returns". Daemons that hit this on reconcile-time HNSW reload would silently lock up. The save path's lock is also unbounded but that's the writer's own choice; the reader path inheriting the same wait makes a single misbehaving writer take down all readers.
- **Suggested fix:** Use `try_lock_shared` with a bounded retry loop (e.g. 5 attempts × 200 ms with jitter) and surface a `HnswError::Internal("save in progress, try again")` when it expires. Daemons can retry; CLI users see a clear error instead of an opaque hang. Existing test infrastructure in `src/hnsw/persist.rs` tests at lines 1762+ already exercises lock contention — extend with a stuck-writer test.

#### DS-V1.36-7: `Store::close()` still uses `wal_checkpoint(TRUNCATE)` — can deadlock under V1.36.2 PR #1450 conditions
- **Difficulty:** easy
- **Location:** src/store/mod.rs:1289-1298 (`Store::close`)
- **Description:** PR #1450 (recent main commit `4b3fd41e`) replaced the `Store::drop` checkpoint from TRUNCATE → PASSIVE specifically because TRUNCATE acquires the SQLite EXCLUSIVE lock and can stall under WSL 9P when readers are present, occasionally blowing past the 30s busy_timeout (per `git log` and src/store/mod.rs:1440-1447). `Store::close()` — the explicit caller-controlled shutdown path — still issues `PRAGMA wal_checkpoint(TRUNCATE)` with no timeout. If a user calls `close()` while another reader handle (e.g. an explicit `cqs stats` invocation, or the watch daemon) holds the WAL, close blocks on the EXCLUSIVE lock. Under WSL 9P this is the same failure mode the drop-path fix addressed. close() is called from the `--force` rebuild path (after this audit's DS-V1.36-4 is applied) and from `cqs serve` shutdown — both places where a stuck checkpoint becomes operator-visible.
- **Suggested fix:** Wrap the close-path checkpoint in a `tokio::time::timeout` (e.g. 30s) and downgrade to PASSIVE on timeout, mirroring the drop-path treatment. Or accept that close is "best-effort fully-blocking" and document the timeout expectation; either way the unbounded TRUNCATE is the same hazard PR #1450 fixed elsewhere.

#### DS-V1.36-8: `migrate_v18_to_v19` orphan-drop threshold uses lossy `f64 → i64` cast
- **Difficulty:** easy
- **Location:** src/store/migrations.rs:738-754 (orphan threshold computation)
- **Description:** `let threshold = (before_rows as f64 * 0.10) as i64;`. For very large `before_rows` (≥ 2^53 ≈ 9 quadrillion) the f64 multiplication loses precision, producing an incorrect threshold. More relevantly: for `before_rows < 10`, threshold is `0` — meaning a single dropped orphan trips the `error!` log path, spamming the user with "more than 10% dropped" warnings on small DBs that have any orphan at all. The intent is "10% or more is suspicious, otherwise log routinely". Saturating-cast or integer arithmetic avoids both edge cases.
- **Suggested fix:** Use integer math: `let threshold = before_rows / 10;` and require `dropped > threshold` to fire the error path. The min-corpus case (before_rows=5, threshold=0, dropped=1) then falls through to the `warn` arm rather than `error`. Float arithmetic isn't justified here — the percentage is a single fixed constant.

#### DS-V1.36-9: `INSERT OR REPLACE` on `llm_summaries` triggers `bump_splade_on_chunks_delete` cascade if FK mistakenly added
- **Difficulty:** medium
- **Location:** src/store/chunks/crud.rs:520 (llm_summaries upsert) — preventive
- **Description:** Currently safe: `llm_summaries` has no FK to `chunks`. But the `INSERT OR REPLACE` form is well-known to issue an implicit DELETE-then-INSERT, which fires ON DELETE triggers and ON DELETE CASCADE foreign keys. If a future migration adds `FOREIGN KEY (content_hash) REFERENCES chunks(content_hash) ON DELETE CASCADE` (a tempting addition for cleanup), every summary refresh would silently delete and re-insert, firing the v20 `bump_splade_on_chunks_delete` trigger via the cascade chain — bumping `splade_generation` and invalidating the on-disk SPLADE index on every summary write. The latent footgun is the trigger's broad scope: any future FK to chunks gets caught. P4-6 in the v1.33 triage flagged this conceptually ("INSERT OR REPLACE cascade enforced only by call-site convention") but the v20 trigger added a new cascade target since.
- **Suggested fix:** Replace the `INSERT OR REPLACE` with `INSERT … ON CONFLICT(content_hash, purpose) DO UPDATE SET ...` so the upsert path is a true UPDATE on conflict and never fires DELETE triggers. Same change PR #1342 made for `chunks` (per the comment at src/store/chunks/crud.rs:215). Idempotent semantics, no behavior change today, no future-FK footgun.

#### DS-V1.36-10: Migration backup `prune_old_backups` skips on `read_dir` failure — orphan disk burn on permission error
- **Difficulty:** easy
- **Location:** src/store/backup.rs:222-243 (`prune_old_backups`)
- **Description:** The prune step swallows `std::fs::read_dir(dir)` errors with a `tracing::warn!` and returns `Ok(())`. So the next migration runs, drops another `.bak-v*-v*-*.db` (potentially several hundred MB on a large project), the prune step warns again and returns Ok, and the `.cqs/` directory accumulates one new backup per migrate forever. KEEP_BACKUPS=3 is enforced only when the read succeeds. The most likely cause of read_dir failure is a transient permission issue (CI containers, NFS mount glitch) — exactly the conditions where backups also accumulate fastest because migrations re-run repeatedly. After 50 runs the user has 50 × 200 MB = 10 GB of backups in `.cqs/` with no signal beyond a debug log line.
- **Suggested fix:** Surface read_dir failures as `StoreError::Io` to the caller, who already classifies prune failure as "non-fatal — the user's DB is at the correct version" (src/store/migrations.rs:122-126). The error can still be downgraded to a warn at the caller, but at least one log site at `error!` would let operators notice the accumulation. Alternative: emit a `tracing::error!` with the directory size when prune was skipped, so monitoring can alert.


<!-- ===== docs/audit-scratch/batch2-performance.md ===== -->
## Performance

#### PERF-V1.36-1: `fetch_candidates_by_ids_async` rebuilds id-position map per call instead of hashing on placeholder bind
- **Difficulty:** easy
- **Location:** src/store/chunks/async_helpers.rs:86-90
- **Description:** Every search hits this hot path. For ~200-500 candidate ids it allocates a `HashMap<&str, usize>` plus a `Vec<Option<(CandidateRow, Vec<u8>)>>` of `ids.len()` slots, then iterates rows to drop them into slots. The current code is fine *complexity*-wise (linear), but the slot vector wastes one `Option<>` discriminant per id (~32 bytes × N) and every miss leaves a `None` that gets walked again in `flatten()`. With ~32k candidates this is ~1MB of `Option` discriminants pushed across cache lines, then walked twice. A flat `Vec::with_capacity(ids.len())` plus a single re-sort by `id_pos.get(&candidate.id)` is cheaper and a smaller working set; or pre-allocate `Vec<MaybeUninit<...>>` with a presence bitmap. Comment claims this is the "fast" path but `Vec<Option<T>>` reorder isn't free and the `flatten()` collect loses the original allocation anyway.
- **Suggested fix:** Build `Vec<(CandidateRow, Vec<u8>)>` with `with_capacity(ids.len())`, then `sort_unstable_by_key(|(c, _)| id_pos[&c.id])` once at the end. One alloc, no `Option` discriminant churn, deterministic.

#### PERF-V1.36-2: `lang_set` / `type_set` rebuild from enum-to-string-to-lowercase on every search call
- **Difficulty:** easy
- **Location:** src/search/query.rs:849-856
- **Description:** Per search, `filter.languages` and `filter.include_types` are converted to `HashSet<String>` via `iter().map(|l| l.to_string().to_lowercase()).collect()`. Both `Language` and `ChunkType` are enums whose `Display` yields canonical lowercase already (the comment at line 861-865 explicitly says "DB values are already canonical lowercase from `Language::to_string` / `ChunkType::to_string`"). The `to_lowercase()` allocates a fresh `String` per variant for no reason, and rebuilding the set per search is wasteful — enums have `&'static str` representations the set could store as `&'static str`. A 50-search session with 10-language filters does 500 needless heap allocations.
- **Suggested fix:** Make `Language::as_str` and `ChunkType::as_str` return `&'static str`, then build `HashSet<&'static str>` from `filter.languages`/`filter.include_types` (no allocation). Compare via `lang_set.contains(candidate.language.as_str())`. Drops the `.to_lowercase()` and `String` allocs entirely.

#### PERF-V1.36-3: `ChunkRow::from_row` does ~16 column-name string lookups per row
- **Difficulty:** medium
- **Location:** src/store/helpers/rows.rs:72-100, also `from_row_lightweight` at 107-130
- **Description:** Every column read (`row.get("id")`, `row.get("origin")`, ...) does a linear scan of `SqliteRow::column_index_by_name` against the column-name list. For a 16-column SELECT with N rows this is `16N` strcmps. Search hydrates 100-500 rows per query; over 50 searches that's 80k-400k strcmps purely for column-name resolution. Indexed access via `row.get(0)`, `row.get(1)` etc. would skip the lookup entirely; the SELECT order is fixed in the same module (async_helpers.rs:34-36 and 92-96).
- **Suggested fix:** Switch to ordinal `row.get::<_, _>(0)`, `row.get(1)` etc. in `ChunkRow::from_row` and `CandidateRow::from_row`. Keep the SELECT column order pinned in a const string constant adjacent to the `from_row` so the contract is local.

#### PERF-V1.36-4: `fetch_and_assemble` does double hashmap lookup + clone per gathered chunk
- **Difficulty:** easy
- **Location:** src/gather.rs:417-420
- **Description:** Anti-pattern `if seen_ids.contains(&r.chunk.id) { continue; } seen_ids.insert(r.chunk.id.clone());` does two hash probes and clones the id String regardless of outcome. Same anti-pattern at src/llm/doc_comments.rs:333-337 and src/cli/commands/graph/explain.rs:251 / 442. `HashSet::insert` returns `bool` already.
- **Suggested fix:** `if !seen_ids.insert(r.chunk.id.clone()) { continue; }` — single probe, clone happens only when actually inserting. Even better: `if !seen_ids.contains(r.chunk.id.as_str()) { seen_ids.insert(r.chunk.id.clone()); chunks.push(...); }` if `seen_ids` were `HashSet<String>` keyed by `&str` borrow.

#### PERF-V1.36-5: `extract_imports_regex` allocates `String` per probed line even when already in seen set
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:807-820
- **Description:** Inner loop calls `seen.insert(trimmed.to_string())` after the regex match. `to_string()` runs *before* `insert()` decides whether the entry is new — so duplicate import lines (very common — every chunk in a file repeats the `use ...` block) allocate a `String`, hash it, find the duplicate, then drop. For a file with 10 chunks each repeating 20 imports that's 180 wasted `String` allocs per call. `cqs where`/`cqs task` runs this on every call, with no compiled-regex caching for the seen set itself.
- **Suggested fix:** Use the standard `if !seen.contains(trimmed) { seen.insert(trimmed.to_string()); imports.push(trimmed.to_string()); }` — one alloc on first sight, zero on duplicates. Same pattern as PERF-V1.36-4 above.

#### PERF-V1.36-6: `BuildHierarchy` rebuilds `visited_names: Vec<String>` from already-interned `Arc<str>` keys
- **Difficulty:** easy
- **Location:** src/serve/data.rs:739
- **Description:** `let visited_names: Vec<String> = depth_by_name.keys().map(|s| s.to_string()).collect();` clones every Arc<str> to a fresh String. Hot path in the daemon hierarchy view — N can be ~10k for a heavy hub. Then those strings are used purely as bind-keys (`q.bind(n)`) and HashMap keys (`name_to_first_id: HashMap<String, String>`) downstream. We had Arc<str> already; SQLite bind accepts `&str` so the conversion is pointless.
- **Suggested fix:** Keep `Vec<Arc<str>>` (or `Vec<&str>` borrowed from the keys) and bind via `n.as_ref()`. For `name_to_first_id`, key by `Arc<str>` so the chain stays alloc-free. Saves ~10k String clones on a deep hierarchy.

#### PERF-V1.36-7: `search_by_names_batch` clones chunk_id and original_name strings per FTS row, even on miss
- **Difficulty:** medium
- **Location:** src/store/chunks/query.rs:436-450
- **Description:** For each light_row the inner loop does `ids_to_fetch.push(id.clone())` and `matched.push((id.clone(), original_name.to_string(), score))` — two String clones per matched row, plus `result.entry(original_name.to_string()).or_default()` which always allocates the key (even on existing entries). Phase 3 then does `chunk_row.clone()` (line 466) — full ChunkRow clone including content/doc/signature for every result row. Caller is `gather`'s `fetch_and_assemble`, hit per gather call.
- **Suggested fix:** Use `result.raw_entry_mut()` (or precompute `original_name: String` once per batch entry, share via Arc), and replace `chunk_row.clone()` with `chunk_row` move — `full_chunks` is consumed after this loop, so use `into_iter` + `HashMap::remove(&id)` instead of `get(&id).clone()`. Same pattern as `final_scored.into_iter().filter_map(rows_map.remove(&id))` at query.rs:367.

#### PERF-V1.36-8: `fresh_sentinel_nonce` calls `format!("{:02x}", b)` 16 times → 16 String allocations per LLM prompt
- **Difficulty:** easy
- **Location:** src/llm/prompts.rs:33-41
- **Description:** Generates a 32-char hex nonce by looping over 16 bytes and calling `hex.push_str(&format!("{:02x}", b))`. Each iteration allocates a 2-char String, copies into hex, drops. For batch summarization (#1108-related) where prompts are built per-chunk, this fires on every prompt — N×16 allocations. `write!` into `hex` with `core::fmt` would skip the per-byte allocations.
- **Suggested fix:** Use `std::fmt::Write::write!(&mut hex, "{:02x}", b).unwrap()` (infallible into `String`) or `hex.push(hex_char(b >> 4)); hex.push(hex_char(b & 0xf));` for zero allocs. Same pattern at src/cli/commands/io/reconstruct.rs:72 and src/cli/commands/train/train_pairs.rs:37,43 — `out.push_str(&format!(...))` is the giveaway. P3-41 from v1.33.0 was supposedly fixed (#1363) but new sites have landed since.

#### PERF-V1.36-9: `SpladeIndex::search_with_filter` clones every id pushed into the heap
- **Difficulty:** medium
- **Location:** src/splade/index.rs:248-252
- **Description:** After scoring `~min(corpus, query×256)` ≈ ~18k entries, the heap-feed loop does `heap.push(id.clone(), score)` for every (id, score) pair, regardless of whether the heap will accept the entry. With k=200 and 18k scored, that's ~17800 String clones that get immediately dropped inside `BoundedScoreHeap::push` (which always takes `String` by value). At 32-char chunk ids, ~570KB of churn per search.
- **Suggested fix:** Add `BoundedScoreHeap::push_lazy<F: FnOnce() -> String>(&mut self, score: f32, id_fn: F)` that only invokes the closure when the score crosses the eviction boundary. Then call `heap.push_lazy(score, || id.to_string())` — clones only happen for the ~k entries that survive. Same pattern would help at src/serve/data.rs:898-906 where `caller_id.clone()` and `callee_id.clone()` fire per row for the dedup HashSet.

#### PERF-V1.36-10: MMR re-rank converts `Vec<SearchResult>` → `Vec<Option<SearchResult>>` → `Vec<SearchResult>` via two extra collects
- **Difficulty:** easy
- **Location:** src/search/query.rs:435-443
- **Description:** Hot path inside `finalize_results`. To reorder by MMR picks, the code does `let originals: Vec<Option<SearchResult>> = results.into_iter().map(Some).collect::<Vec<_>>(); let mut originals = originals; for &i in &picks { ... }`. That's an extra Vec allocation (Some-wrapping every result, ~Option discriminant per element) plus a redundant rebind. With `Vec<SearchResult>` containing 100-200 results carrying full content/doc, the wrapping is ~24 bytes × N of needless allocation.
- **Suggested fix:** Use `mem::take(&mut results[i])` with `Default::default()` placeholder, OR use `Vec<MaybeUninit<SearchResult>>` and `assume_init` after picks, OR build a permutation `picks: &[usize]` and apply with `swap`-based reorder in place. Cleanest: collect picks into `BTreeMap<usize, ()>`, then drain `results.into_iter().enumerate().filter_map(|(i, r)| picks.get(&i).map(|_| r)).collect()` — one alloc total, no Option wrapping.

#### PERF-V1.36-11: `scout` looks up `stale_set` via double-allocating PathBuf-to-String round trip per file
- **Difficulty:** easy
- **Location:** src/scout.rs:284
- **Description:** `stale_set.contains(&file.to_string_lossy().to_string())` allocates twice per file (once for the `Cow<str>` from `to_string_lossy`, once for the explicit `to_string()`). On Windows the Cow is always Owned anyway. With 50 files in a scout result, that's 100 allocations purely for set lookup. `HashSet<String>` supports `contains::<str>(&str)`, so the second `to_string()` is unneeded.
- **Suggested fix:** `stale_set.contains(&*file.to_string_lossy())` or `stale_set.contains(file.to_string_lossy().as_ref())`. Drops the second alloc. For Linux (Cow::Borrowed common case) drops both.

#### PERF-V1.36-12: `vec!["?"; n].join(",")` placeholder construction allocates Vec + Vec<&str> + final String per batch
- **Difficulty:** easy
- **Location:** src/serve/data.rs:777, 871-872 (and likely other call sites)
- **Description:** Each batch builds `placeholders` via `vec!["?"; batch.len()].join(",")` — allocates a `Vec<&'static str>` of N elements, then joins. The hierarchy edge SQL (line 871-872) does this twice per inner-loop iteration, on N² batches. For deep hierarchies (visited_names > 32k) the cartesian product is many sub-queries; each pays double placeholder construction. `make_placeholders` (used in store/chunks/) skips the Vec by writing `?,?,?...` directly with `String::with_capacity(2*n)`.
- **Suggested fix:** Use the existing `crate::store::helpers::make_placeholders(n)` consistently — it's already optimized and used elsewhere. Audit grep for `vec!["?"` usages and replace with the helper.


<!-- ===== docs/audit-scratch/batch2-resources.md ===== -->
## Resource Management

#### RM-V1.36-1: `truncate_incomplete_line` slurps entire training-data JSONL into memory
- **Difficulty:** easy
- **Location:** src/train_data/checkpoint.rs:56-77
- **Description:** `truncate_incomplete_line` calls `fs::read(path)` on the training-data output JSONL, then walks it in-memory just to find the last newline. Training-data JSONL files routinely run multi-GB (the doc-string says it's used for "crash recovery: partial JSONL lines"). Resume mode (`--resume`) on a 5-10 GB JSONL allocates a 5-10 GB Vec<u8> at startup, peak heap = 2× file size during the in-memory truncate path. Easy DoS surface and a real OOM on agent workstations with the v3.v2 fixture-scale corpora.
- **Suggested fix:** Open file, `seek(SeekFrom::End(-N))` for N=64 KiB, scan the tail buffer for the last `\n`, then `set_len(end_offset)` via `File::set_len`. Constant memory regardless of file size.

#### RM-V1.36-2: `pdf_to_markdown` captures unbounded subprocess stdout via `.output()`
- **Difficulty:** easy
- **Location:** src/convert/pdf.rs:20-25
- **Description:** `Command::new(python).arg(script).arg(path).output()` buffers the entire converter stdout in memory before returning. A 200 MB image-light PDF produces an order of magnitude more text than its bytes; a hostile PDF (or runaway pymupdf4llm) can produce arbitrary stdout that lands as a single `Vec<u8>` in our address space. There's no per-process cap and no per-call timeout — sibling subprocess code in `train_data/git.rs:167-211` already does the right thing with `Command::spawn` + `.take(max+1).read_to_end`.
- **Suggested fix:** Mirror the `train_data::git_diff_tree` pattern: spawn with `.stdout(Stdio::piped())`, wrap in `.take(max+1)`, kill child on overrun, surface a `ConvertError::OutputTooLarge`. Same env-overridable cap pattern (`CQS_PDF_MAX_BYTES`).

#### RM-V1.36-3: Position-IDs build allocates one throwaway Vec per row in the batch
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:1238-1241
- **Description:**
  ```
  let mut pos_data: Vec<i64> = Vec::with_capacity(texts.len() * max_len);
  for _ in 0..texts.len() {
      pos_data.extend((0..max_len as i64).collect::<Vec<i64>>());
  }
  ```
  The inner `.collect::<Vec<i64>>()` allocates a fresh Vec just to immediately consume it via `extend`. For a Qwen3-Embedding-4B batch of 128 × 2048 = 128 throwaway 16 KiB allocations per call. Also `texts.len() * max_len` is unchecked — `texts.len() * max_len` could overflow on 32-bit (cqs is 64-bit so practically fine, but the pattern is `saturating_mul` everywhere else).
- **Suggested fix:** `pos_data.extend(0..max_len as i64)` directly inside the loop (range iter is `Iterator` already). Use `saturating_mul` on the with_capacity arg for consistency with the rest of the codebase.

#### RM-V1.36-4: Watch-loop pre-bind probe `UnixStream::connect` has no timeout
- **Difficulty:** easy
- **Location:** src/cli/watch/mod.rs:540
- **Description:** Before binding the daemon socket, the watch loop does `UnixStream::connect(&sock_path)` to detect a peer daemon. No `Duration` is configured anywhere on this connect — std uses an OS default (Linux `SO_SNDTIMEO`/`SO_RCVTIMEO` unset → blocking). If the previous daemon is wedged in shutdown (mid-checkpoint, in TIME_WAIT analogue, or `accept` queue full), the connect blocks indefinitely and `cqs watch --serve` hangs at startup with no diagnostic. Compare `dispatch.rs:333-359` and `socket.rs:77-88` which BOTH set explicit timeouts.
- **Suggested fix:** Use `UnixStream::connect_timeout` (or set `set_nonblocking(true)` immediately after connect with a brief poll) and treat ETIMEDOUT as "no live peer, proceed with bind". Same 5 s default as `resolve_daemon_timeout_ms()`.

#### RM-V1.36-5: `Command::output()` in chm.rs / convert/mod.rs / train_data/mod.rs swallows whole subprocess output
- **Difficulty:** easy
- **Location:** src/convert/chm.rs:33-47, src/convert/mod.rs:65, src/train_data/mod.rs:528
- **Description:** Same shape as RM-V1.36-2: every `Command::output()` site materializes both stdout and stderr in memory unbounded. chm.rs nullifies stdout but pipes stderr unbounded; convert/mod.rs's binary-existence probe pipes both. A misbehaving 7z that endlessly emits errors → unbounded `Vec<u8>` per invocation. Fewer bytes per call than the PDF case, but same shape and no per-process cap.
- **Suggested fix:** For probes (existence checks) use `.stderr(Stdio::null())`. For real conversions, use the spawn+take pattern from `git.rs`. Single `bounded_output(cmd, max)` helper would unify all five sites.

#### RM-V1.36-6: `add_reference_to_config` / similar config-write paths re-read full file under lock
- **Difficulty:** easy
- **Location:** src/config.rs:820-867 (and analogous remove path ~line 956)
- **Description:** The atomic config-update code path opens the file, takes an exclusive flock, then reads-modifies-writes. The size cap (RM-V1.33-1) is in place, but the read+TOML parse+full-rewrite cycle materializes 3 copies of the config in memory simultaneously (raw `String` content, parsed `toml::Table`, serialized output). With `MAX_CONFIG_SIZE=1 MiB` (default) this is fine; if an operator overrides via env it's cubic in the cap. More importantly, the lock is held across blocking reqwest calls in callers (validation), pinning the file for tens of seconds.
- **Suggested fix:** Read+parse under the flock, drop the flock before any network I/O, re-acquire flock for the final atomic-write phase only. Standard "minimize critical section" refactor — affects `add_reference_to_config` and `remove_reference_from_config` symmetrically.

#### RM-V1.36-7: `BufReader::lines()` in display.rs allocates per-line with no per-line cap
- **Difficulty:** easy
- **Location:** src/cli/display.rs:204-208, src/cli/display.rs:635-639
- **Description:** Both `read_window_lines` paths use `BufReader::new(f).lines().take(limit)`. `BufRead::lines()` allocates a fresh `String` per line via `read_line` — a single pathological source file with a 500 MB line (minified JS bundle, generated lockfile, single-line WASM disassembly) becomes a 500 MB heap allocation even when the caller only wants `limit=20` lines around `line_start`. The `take(limit)` short-circuits the iterator after N lines but doesn't bound the per-line `String` size.
- **Suggested fix:** Use `read_until(b'\n', &mut buf)` with a per-line `take` cap (e.g. 1 MiB) or check `buf.len()` and skip lines exceeding the cap, replacing them with a `[line truncated]` marker. Same pattern other defensive readers already use.

#### RM-V1.36-8: Daemon accept loop polls with 100 ms blocking sleep — wakeup latency on shutdown
- **Difficulty:** easy
- **Location:** src/cli/watch/daemon.rs:212-213
- **Description:** `Err(WouldBlock) => std::thread::sleep(Duration::from_millis(100))` — the accept loop sleeps a flat 100 ms when no client is waiting. On SIGTERM/Ctrl-C the `daemon_should_exit` check at top of loop only fires once per accept tick, so the daemon exit latency is **up to 100 ms** + the time for whatever's currently in-flight to drain. Not a real DoS, but it accumulates: every accept-loop iteration that lands in WouldBlock costs a thread-park syscall. With the 60-second idle-sweep tick window, that's 600 wakeups/min on an otherwise-idle daemon — 600 wasted scheduler trips/min × N daemon processes on the workstation.
- **Suggested fix:** `epoll_wait` / `mio` / `poll` on the listener fd with `daemon_should_exit` checked between events. Or at minimum, raise the sleep to 500 ms — agents poll for fresh on the order of seconds, no need for 10 Hz wakeups when idle.

#### RM-V1.36-9: HNSW build holds full `id_map` Vec<String> in memory at peak
- **Difficulty:** medium
- **Location:** src/hnsw/build.rs:169-209
- **Description:** `build_with_dim_streaming` pre-allocates `Vec::with_capacity(capacity)` for `id_map` where `capacity` is the chunk count fetched from the store. At ~80 chars/chunk-id × 1M chunks = 80 MB just for the id strings — on top of the HNSW graph itself. The streaming-batches design correctly avoids holding all embeddings simultaneously, but `id_map` is the inverse: every entry is retained for the whole build. For very large corpora (the SPLADE-Code 0.6B / Qwen3-4B target), this is the largest single allocation outside the HNSW graph.
- **Suggested fix:** `Vec<Arc<str>>` halves the per-entry overhead vs `Vec<String>` (no separate len/capacity per entry). Or compress: `Vec<u32>` indices into a deduplicated string-arena. Real fix (multi-PR) is to write the id_map directly to disk as it's built and mmap on load — same shape as the embeddings_arena work.

#### RM-V1.36-10: `Vec::with_capacity(chunk_count * dim)` in CAGRA build is unchecked multiplication
- **Difficulty:** easy
- **Location:** src/cagra.rs:746-747
- **Description:** `Vec::with_capacity(chunk_count * dim)` directly multiplies — `chunk_count` is store-controlled (could be in the millions) and `dim` is model-controlled (could be 4096 for Qwen3-4B). On 64-bit the practical overflow risk is gone, but the *allocation* is unchecked: 1M chunks × 4096 dim × 4 B/f32 = 16 GiB. The `cagra_max_bytes` check at line 736-744 happens **before** these `with_capacity` calls, so OOM-via-allocator is gated. But a corrupt store reporting `chunk_count = usize::MAX` slipping through `embedding_count()` would still hit `Vec::with_capacity(usize::MAX)` → panic. Belt-and-suspenders: the same `try_into::<usize>` + sanity-bound pattern as `splade/index.rs:653-664` should apply here.
- **Suggested fix:** Add an upper bound assertion (`chunk_count <= 1<<28` say, matching the SPLADE pattern) before the `with_capacity` calls. Or prefer `Vec::new()` + `extend_from_slice` per batch (the loop already streams batches, the up-front `with_capacity` is the only reason this matters).


<!-- ===== docs/audit-scratch/batch2-test-coverage-happy.md ===== -->
## Test Coverage (happy path)

#### TC-HAP-V1.36-1: `serve::data::build_stats` has no positive test
- **Difficulty:** easy
- **Location:** src/serve/data.rs:1109 — `build_stats` (1 caller: `handlers::stats`)
- **Description:** All sibling builders in `serve/data.rs` (`build_graph`, `build_chunk_detail`, `build_hierarchy`, `build_cluster`) have populated-store positive tests under the `TC-HAP-1.29-1` block in `src/serve/tests.rs:1010-1190`. `build_stats` is the only one without a direct positive test — only the `/api/stats` HTTP layer exercises it, and only against a tiny fixture without verifying the four numeric fields (`total_chunks`, `total_files`, `call_edges`, `type_edges`). Schema regressions (`call_edges` vs prior `total_call_edges`, missing `type_edges` after the type-edge migration) would slip through.
- **Suggested fix:** Add `build_stats_returns_correct_counts_for_populated_store` next to `build_chunk_detail_returns_callers_callees_tests`. Insert N chunks across M distinct origin files, upsert K function_calls and L type_edges, then assert `(total_chunks, total_files, call_edges, type_edges) == (N, M, K, L)`.

#### TC-HAP-V1.36-2: `Store::get_callers_with_context` has no direct unit test
- **Difficulty:** easy
- **Location:** src/store/calls/query.rs:150 — `get_callers_with_context` (callers: `impact::analysis::analyze_impact`, plus `_batch` variant in `impact::diff`)
- **Description:** The function joins call-edges with chunks/snippets to return `CallerInfo` with `call_line` and snippet context — load-bearing for `cqs impact`. Only indirectly tested via `tests/impact_test.rs::analyze_impact`. A regression in JOIN order, snippet truncation, or `call_line` extraction would reach `cqs impact` JSON output without anyone catching it. The simpler `get_callers_full` at line 14 has test coverage in `tests/store_calls_test.rs:215`; this richer variant doesn't.
- **Suggested fix:** Add to `tests/store_calls_test.rs`: insert two chunks A and B with calls A→B at known line numbers, call `store.get_callers_with_context("B")`, assert the returned `CallerInfo` has the expected `name`, `file`, `line`, `call_line`, and a non-empty `snippet`.

#### TC-HAP-V1.36-3: `get_callers_full_batch` and `get_callees_full_batch` untested
- **Difficulty:** easy
- **Location:** src/store/calls/query.rs:239 and :294 (callers: `cli::enrichment::enrich_chunks`, `cli::commands::io::context::build_full_data`, `tests/pipeline_eval.rs`)
- **Description:** Two `pub` batch variants on a hot path (page-render and context-pack stages). `tests/pipeline_eval.rs` calls them with `unwrap_or_default()`, so silent regressions like "returns empty map for one of N names" never trip a test. The non-batch `get_callers_full` has explicit tests in `tests/store_calls_test.rs`; the batch versions don't.
- **Suggested fix:** In `tests/store_calls_test.rs`, after the `store.upsert_function_calls` fixture, call both batch fns with a `Vec<&str>` of three names where one is unknown. Assert the unknown name maps to `Vec::new()` (not missing key) and the others have the expected callers/callees.

#### TC-HAP-V1.36-4: `cli::commands::io::context::build_compact_data` and `build_full_data` untested
- **Difficulty:** easy
- **Location:** src/cli/commands/io/context.rs:25 (`build_compact_data`) and :130 (`build_full_data`) — both `pub(crate)`, exclusively used by `cmd_context`
- **Description:** `cmd_context` is exercised only at the JSON-shape level (`compact_to_json`, `full_to_json`, `summary_to_json` have `hp1_*` tests), but the data-fetching builders that populate those shapes from the store have zero direct tests. Both contain non-trivial logic: path normalization (PB-V1.29-1), `bail!` on empty origin, `get_caller_counts_batch`/`get_callee_counts_batch` reduction. A breakage in the normalization or the empty-path guard would only surface as a `cqs context` regression in production.
- **Suggested fix:** Add a `tc_hap_build_compact_data_*` test that opens an in-memory store, upserts two chunks under `src\\foo.rs` (Windows-style backslashes), calls `build_compact_data(&store, "src\\foo.rs")`, and asserts both chunks come back with non-zero caller/callee counts wired up. Mirror for `build_full_data`.

#### TC-HAP-V1.36-5: `apply_ci_token_budget` (`pub(crate)` entry point) has no direct test
- **Difficulty:** easy
- **Location:** src/cli/commands/review/ci.rs:64 — `apply_ci_token_budget` (caller: `cli::batch::handlers::analysis:206`)
- **Description:** `apply_ci_token_budget` is a `pub(crate)` shim over the local `apply_token_budget(_, _, json=true)`. The sibling `apply_token_budget_public` in `diff_review.rs:70` has tests at `:388` and `:407`, but the CI variant — used by the batch pipeline for `cqs ci --tokens N` — has none. The `json=true` branch enables `JSON_OVERHEAD_PER_RESULT` which inflates per-item token cost; a regression in that constant would silently fit fewer items into the budget for batch CI but not for review.
- **Suggested fix:** Add `tests` mod to `ci.rs` with `test_apply_ci_token_budget_truncates_callers_and_tests` and `test_apply_ci_token_budget_zero_returns_zero_items` mirroring the `diff_review.rs:test_apply_token_budget_*` shape, but pinning `json=true` accounting.

#### TC-HAP-V1.36-6: `HnswIndex::search` (unfiltered) has no direct unit test
- **Difficulty:** easy
- **Location:** src/hnsw/search.rs:23 — `HnswIndex::search` (callers: `Store::search_*` family)
- **Description:** `search_filtered` has TC-17 tests (referenced at `src/hnsw/build.rs:487`) and dim-mismatch tests in `tests/embedder_dim_mismatch_test.rs`. The unfiltered `search` entry point has no direct test — it's only reached through the higher-level `Store::search` which mixes RRF, BM25, and reranker concerns. The empty-query and dim-mismatch early-returns at lines 53 and 65, and the non-finite filter at line 82, deserve dedicated coverage at the HNSW layer rather than smuggled in through 6 wrapper layers.
- **Suggested fix:** Add to `src/hnsw/mod.rs` tests: `test_hnsw_search_empty_index_returns_empty`, `test_hnsw_search_dim_mismatch_returns_empty`, `test_hnsw_search_nonfinite_query_returns_empty`, `test_hnsw_search_returns_top_k_in_score_order`. Use `make_test_embedding` already imported at `:700`.

#### TC-HAP-V1.36-7: `cli::commands::io::context::pack_by_relevance` has no test
- **Difficulty:** easy
- **Location:** src/cli/commands/io/context.rs:349 — `pack_by_relevance` (caller: `cmd_context` token-budgeting path)
- **Description:** Token-budget packing for the `cqs context --tokens N` flag. The companion `build_token_pack` private at `:448` is exercised through the `cmd_context` integration test, but `pack_by_relevance` — which applies the relevance-weighted ordering — has no direct test. Score ordering or saturation bugs in the relevance heuristic land in production output without being caught.
- **Suggested fix:** Add `pack_by_relevance_orders_by_score` test in `context.rs` `tests` mod. Build a `Vec<ChunkSummary>` with three chunks (high/mid/low scores), call `pack_by_relevance`, assert the high-score chunk is first and the low-score is last (or dropped if budget excludes it).

#### TC-HAP-V1.36-8: `cli::pipeline::embedding::prepare_for_embedding` has no test despite being a major orchestrator
- **Difficulty:** medium
- **Location:** src/cli/pipeline/embedding.rs:26 — `prepare_for_embedding` (callers: `gpu_embed_stage:247`, `cpu_embed_stage:461`)
- **Description:** 130-line function that does five logical steps (windowing, global cache, store cache, partition, NL description). The sibling `create_embedded_batch` at `:159` has four positive tests in `src/cli/pipeline/mod.rs:277-360`. `prepare_for_embedding` has zero direct tests — silent regressions in the cache hit/miss split or the windowing→hash chain only surface as eval-recall regressions. (Note: the pre-existing R@5 regression noted in MEMORY.md between 2026-04-25 and 2026-04-30 lives somewhere in this region of code.)
- **Suggested fix:** Add `test_prepare_for_embedding_separates_cached_and_uncached` and `test_prepare_for_embedding_uses_global_cache_when_available` next to the `create_embedded_batch` tests. Use a fake/in-memory `EmbeddingCache` with one pre-seeded `(content_hash, model_fp)` entry; assert that one chunk lands in `cached` and the other in `to_embed`.

#### TC-HAP-V1.36-9: Daemon GC entry points untested
- **Difficulty:** medium
- **Location:** src/cli/watch/gc.rs:114 (`run_daemon_startup_gc`) and :218 (`run_daemon_periodic_gc`)
- **Description:** Both functions are `pub(super)` and called from `cli/watch/mod.rs:1051` and `:1450`. The lower-level `prune_last_indexed_mtime` is well-tested in `cli/watch/tests.rs:688-803`. The two big GC drivers — which orchestrate `Pass 1: drop chunks for missing files` + `Pass 2: drop chunks for now-gitignored paths` and the periodic origin-cap walker (`DAEMON_PERIODIC_GC_CAP_DEFAULT=1000`) — have no direct test. Bugs in the cap honoring, the gitignore-matcher integration, or the `CQS_DAEMON_STARTUP_GC=0` opt-out only surface in production daemon logs.
- **Suggested fix:** Add to `cli/watch/tests.rs`: `test_run_daemon_startup_gc_prunes_missing_files` (insert 5 chunks for files A,B,C,D,E; delete A and B from disk; run startup GC; assert chunk count drops to 3) and `test_run_daemon_periodic_gc_honors_cap` (set `CQS_DAEMON_PERIODIC_GC_CAP=2`, verify only 2 origins per tick).

#### TC-HAP-V1.36-10: `train_data::cmd_train_data` and `cmd_plan` untested directly
- **Difficulty:** medium
- **Location:** src/cli/commands/train/train_data.rs:7 and src/cli/commands/train/plan.rs:7
- **Description:** `cli_train_review_test.rs` covers `cmd_plan` (P2 #46 (a)) but `cmd_train_data` has only an eval-fixture mention and no integration test. The function calls `cqs::train_data::generate_training_data` and prints a six-field summary; if the underlying generator changes its `TrainingDataStats` shape (rename `commits_skipped` → `commits_filtered`, etc.) the print format silently drifts from what the spec promises and from `cqs train-data --help`.
- **Suggested fix:** Add `tests/cli_train_data_test.rs` (subprocess pattern, like `cli_train_review_test.rs`): set up a tiny git repo with 2 commits, run `cqs train-data --output /tmp/x.jsonl`, assert exit code 0 and that stdout matches `Generated \d+ triplets from 1 repos \(\d+ commits processed, \d+ skipped\)`.

