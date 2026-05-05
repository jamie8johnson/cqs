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

DONE
