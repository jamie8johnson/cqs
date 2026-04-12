# Audit Findings — v1.22.0

Audit date: 2026-04-11

Full 16-category audit run via `.claude/skills/audit` skill. Two batches of 8 parallel auditor agents. Findings appended by each category agent.

## Scope

Current project state: main at 9f2256d (after SPLADE persistence, integrity check skip, eval harness fix, OpenRCT2 spec revision shipped this session). Version 1.22.0 in progress.


---

# Code Quality — v1.22.0 audit

7 findings: 1 actually-dead pub function, 1 missed cache invalidation after PR #895, 1 non-atomic generation bump, near-verbatim duplicate SPLADE loader in CLI vs batch, a missing SPLADE re-encode skip-gate that silently negates the PR #895 perf win, two `try_load_with_ef(None, None)` call sites that pass past-the-project dim instead of `store.dim()`, and a still-unfixed test-helper duplicate triaged as "fixing" in v1.20.0.

## `set_rrf_k_from_config` is dead code, never called by any binary
- **Difficulty:** easy
- **Location:** src/store/search.rs:13-19 (also re-exported at src/store/mod.rs:162)
- **Description:** The function sets a `OnceLock<f32>` that `rrf_k()` at src/store/search.rs:23-35 reads. A grep across the entire crate (including `src/cli`, `src/bin`, all binaries) finds **zero** call sites — only the definition, the re-export, and a doc-comment reference in `src/search/scoring/config.rs:34` that *describes* the pattern. `config::ScoringOverrides::rrf_k` exists and parses from `.cqs.toml`, so a user writing `[scoring]\nrrf_k = 40` gets their value silently ignored. This is the exact "built it but nothing calls it" pattern the MEMORY.md HNSW disaster warns about — config plumbing stopped at the `pub fn`. Triage v1.20.0 listed EXT-5 as "rrf_k not in ScoringOverrides, defer" — but the field exists now; the gap is the CLI wiring, not the config shape.
- **Suggested fix:** Either (a) call `cqs::store::set_rrf_k_from_config(&overrides)` early in `src/cli/definitions.rs` or `src/cli/mod.rs` where other config overrides are applied, inside a test that reads `rrf_k` from a config file and verifies `rrf_k()` returns it; or (b) delete `set_rrf_k_from_config` and the `ScoringOverrides::rrf_k` field together, since the `CQS_RRF_K` env var already works as the override path.

## Batch mode `invalidate_mutable_caches()` forgets `splade_index` — serves stale SPLADE results indefinitely after concurrent reindex
- **Difficulty:** easy
- **Location:** src/cli/batch/mod.rs:177-186 (invalidate) and 281-285 (ensure_splade_index early return)
- **Description:** `invalidate_mutable_caches` clears `hnsw`, `call_graph`, `test_chunks`, `file_set`, `notes_cache`, and `refs`, but not `splade_index` (line 89 field). `ensure_splade_index` at line 281 calls `check_index_staleness` first, then returns early if `self.splade_index.borrow().is_some()`. Once a batch session has loaded the SPLADE index once, an interleaving `cqs index` in another shell bumps `splade_generation` in SQLite, invalidates every other cache via mtime tracking, re-opens the Store — but the in-memory `SpladeIndex` stays. Every subsequent `search --splade` in the batch session serves results from the dropped-in-memory generation; the on-disk `splade.index.bin` is never consulted again this session. The `rebuilt` flag in the log line at 316 cannot detect this because the function returned at 284.
- **Suggested fix:** Add `*self.splade_index.borrow_mut() = None;` to `invalidate_mutable_caches` right next to the other RefCell clears. Add a test in `src/cli/batch/mod.rs#mod tests` that inserts sparse_vectors, calls `ensure_splade_index`, mutates `sparse_vectors` + bumps the generation, touches index.db mtime, and asserts `ensure_splade_index` rebuilds (observe via tracing or return value).

## `prune_orphan_sparse_vectors` deletes + bumps generation in three separate un-transactioned statements (atomicity + lost-update races)
- **Difficulty:** easy
- **Location:** src/store/sparse.rs:229-262
- **Description:** Unlike `upsert_sparse_vectors` (line 26) which wraps everything in `begin_write()` → commit, this function issues three raw `&self.pool` queries: DELETE, SELECT metadata, INSERT metadata. Two concrete failures: (1) a crash or error between DELETE and the metadata update leaves the generation stale — next query re-uses a now-inconsistent on-disk `splade.index.bin` thinking nothing changed, because the header generation still matches the un-bumped store generation. The on-disk index contains postings for chunks no longer in `sparse_vectors`. (2) Concurrent `cqs watch` running `upsert_sparse_vectors` between our SELECT (gen=5) and INSERT (gen=6) loses their bump — they commit gen=6 first, we overwrite with our gen=6, watch's writes effectively become generation-invisible to the next loader. There is also no `WRITE_LOCK` protection because the function bypasses `begin_write()`.
- **Suggested fix:** Wrap all three statements in `begin_write()` → tx → commit, same pattern as `upsert_sparse_vectors`. The generation update should be conditional on `result.rows_affected() > 0` inside the transaction. Add a test that runs `prune_orphan_sparse_vectors` twice with no actual orphans and asserts the generation does not change.

## `cmd_index` unconditionally re-encodes all SPLADE chunks on every run, silently negating the PR #895 persist win
- **Difficulty:** medium
- **Location:** src/cli/commands/index/build.rs:389-568 (the whole SPLADE block), combined with src/store/sparse.rs:127-144 (unconditional generation bump in upsert)
- **Description:** The SPLADE encoding block at build.rs:389 is gated only on `resolve_splade_model_dir().is_some()` — if a SPLADE model is installed, every `cqs index` run (even a no-op run where zero chunk files changed) re-encodes all ~12K chunks via `chunk_splade_texts()` at line 402. The resulting `sparse_vecs` goes into `upsert_sparse_vectors` which DROPs the `idx_sparse_token` secondary index, DELETEs all the old rows, INSERTs identical new ones, recreates the index, and unconditionally bumps `splade_generation` on line 138-144. Next query sees the bumped generation, fails the on-disk `splade.index.bin` load check via `GenerationMismatch`, and rebuilds the in-memory index from SQLite. The 45s rebuild cost that PR #895 was designed to eliminate returns on every pair-of-invocations `cqs index; cqs search`. Notably, `build.rs:535-562` persists the newly-built SpladeIndex on disk, but this is immediately invalidated the next time `cmd_index` runs with unchanged files. Watch mode is not affected because `reindex_files` in src/cli/watch.rs:684 doesn't touch SPLADE, but manual `cqs index` is the common path.
- **Suggested fix:** Before encoding, query `chunk_splade_texts()` and compare against a fingerprint of the existing `sparse_vectors` rows — the simplest version is to select the set of chunk IDs from `sparse_vectors` and check whether it matches `chunk_texts`. If equal, skip the re-encode + upsert entirely. Alternatively (more robust) compute a content hash over `(id, name, signature, doc)` for every chunk and store it alongside `sparse_vectors`; only re-encode chunks whose hash changed. Either path must leave the `splade_generation` counter untouched when nothing changed so the on-disk persist stays valid.

## `HnswIndex::try_load_with_ef` called with `dim=None` in two reference-load sites that already have `store.dim()` available
- **Difficulty:** easy
- **Location:** src/reference.rs:106 and src/project.rs:322
- **Description:** `try_load_named` at src/hnsw/persist.rs:793 does `let load_dim = dim.unwrap_or(crate::EMBEDDING_DIM);` — when a caller passes `None`, HNSW loads using the crate-default dim (1024 for BGE-large). Both call sites have already successfully opened the target Store two lines earlier (reference.rs:93-104, project.rs:320), so `store.dim()` is available. A reference project or cross-project target built with a 768-dim model (E5-base or v9-200k, both production presets per MEMORY.md) silently loads a half-truncated HNSW view of 1024-dim bytes. Cross-project search against a differently-dimensioned peer returns garbage scores. This is the same class of bug as the `build_batched()` / `build_batched_with_dim()` disaster from PR #690 — a convenience wrapper (`None` → default dim) masked a dim mismatch. Contrast with the *correct* pattern at src/cli/commands/search/similar.rs:84 and src/cli/commands/graph/explain.rs:84 which already pass `Some(store.dim())`.
- **Suggested fix:** Change `HnswIndex::try_load_with_ef(&cfg.path, None, None)` → `HnswIndex::try_load_with_ef(&cfg.path, None, Some(store.dim()))` in reference.rs:106 and project.rs:322. Stronger: delete the `dim.unwrap_or(EMBEDDING_DIM)` default at persist.rs:793 and make `dim: Option<usize>` → `dim: usize`, forcing every caller to think. The only risk is breaking compile sites, which a cargo check immediately surfaces.

## `splade_encoder()` and the entire SPLADE-index loader duplicated between `CommandContext` and `BatchContext`
- **Difficulty:** easy
- **Location:** src/cli/store.rs:144-210 vs src/cli/batch/mod.rs:247-320
- **Description:** `CommandContext::splade_encoder` (store.rs:144-164) and `BatchContext::splade_encoder` (batch/mod.rs:253-272) are byte-for-byte identical except for the tracing span name and whether the result wraps in `OnceLock` vs a `RefCell`-initialized `OnceLock`. `CommandContext::splade_index` (store.rs:172-210) and `BatchContext::ensure_splade_index` (batch/mod.rs:281-320) are the same except for the storage container. Both duplicate the generation-read boilerplate, the path join with `SPLADE_INDEX_FILENAME`, the `load_or_build` call, the empty-check early return, and the tracing info emit. Two effects: (1) when someone fixes the missed `splade_index` invalidation bug described above, they'll have to fix it in two places or fix only one and ship a second bug; (2) a future SPLADE load-time improvement (streaming reader, mmap, delta-load) has to be applied twice. The `splade` module is the right owner of this logic.
- **Suggested fix:** Add a free function `cqs::splade::index::open_for_store(store: &Store, cqs_dir: &Path) -> Option<SpladeIndex>` that runs the whole: read generation, build path, `load_or_build`, empty check, return. Both `CommandContext::splade_index` and `BatchContext::ensure_splade_index` become ~5-line wrappers that cache the result (OnceLock / RefCell). Same for `splade_encoder` → `open_for_current_model()` already-ish exists via `resolve_splade_model_dir` but the encoder-construction boilerplate around it is what's duplicated.

## Duplicate `make_named_store` test helper still present despite being marked "fixing" in v1.20.0 triage
- **Difficulty:** easy
- **Location:** src/store/calls/cross_project.rs:278 and src/impact/cross_project.rs:291
- **Description:** CQ-8 in docs/audit-triage-v1.20.0.md:47 is flagged as "fixing" but grep shows both helpers still exist with nearly identical logic (both create a temp dir, open a Store, call `ModelInfo::default()`, init, insert into function_calls). Both files even contain a `// NOTE: similar helper exists in …` comment pointing at each other. This is dead work — the fix was queued then lost. Minor on its own, but worth calling out because the triage file says it's fixed and the next audit cycle would skip it otherwise.
- **Suggested fix:** Move the helper to `src/test_helpers.rs` (already exists, already `#[cfg(test)]`) as `make_named_store_with_calls(name, forward_edges)` taking a superset of both signatures. Update both call sites to import from test_helpers. Retire CQ-8 in the next triage doc.

---

# Documentation — v1.22.0 audit

Eleven findings. README and SECURITY docs carry stale claims about integrity_check behaviour (contradicted by #893), the `.cqs/` file list is missing `splade.index.bin` (introduced by #895), the CHANGELOG [Unreleased] section is empty despite four shipped PRs, CONTRIBUTING.md Architecture Overview is missing three source files that exist on disk (`src/splade/`, `src/search/router.rs`, `src/store/sparse.rs`), the README env-var table is missing eight CQS_* vars and has one wrong default, the README schema version is two versions behind, and two stale doc comments reference old module paths.

## CHANGELOG [Unreleased] empty despite four shipped session PRs
- **Difficulty:** easy
- **Location:** CHANGELOG.md:8-9
- **Description:** `[Unreleased]` is empty. This session shipped #893 (integrity check skip), #894 (eval harness fix), #895 (SPLADE index persistence + new file `splade.index.bin` + schema `metadata.splade_generation` + `CQS_SKIP_INTEGRITY_CHECK` env var), #896 (OpenRCT2 spec rewrite). None are listed. The `[1.22.0]` section is dated 2026-04-09 but Cargo.toml still has `version = "1.22.0"`, so either the session work needs to land in the existing 1.22.0 section or a new unreleased section. This is the same failure mode as DOC-32 from v1.20.0 triage.
- **Suggested fix:** Add an `### Added`, `### Fixed`, and `### Perf` block under `[Unreleased]` covering #893 (integrity_check behaviour change + `CQS_SKIP_INTEGRITY_CHECK`), #894 (eval harness `--` separator + `CQS_EVAL_TIMEOUT_SECS`), #895 (SPLADE index persistence + `splade.index.bin` + `metadata.splade_generation` counter + blake3 body checksum), #896 (OpenRCT2 spec edit).

## SECURITY.md falsely claims integrity_check(1) on every open
- **Difficulty:** easy
- **Location:** SECURITY.md:22
- **Description:** Threat model says `"Database corruption: PRAGMA integrity_check(1) on every database open"`, but `src/store/mod.rs:411-441` now (post-#893) skips the check entirely on read-only opens and runs `PRAGMA quick_check(1)` (not `integrity_check`) on write opens, with an opt-out via `CQS_SKIP_INTEGRITY_CHECK=1`. The claimed protection is strictly weaker than before and the doc overstates what the code delivers.
- **Suggested fix:** Update to: `"Database corruption: PRAGMA quick_check(1) on write opens (opt-out via CQS_SKIP_INTEGRITY_CHECK=1). Read-only opens do not verify — reads cannot introduce corruption and a rebuildable search index does not justify the upfront cost."`

## SECURITY.md missing splade.index.bin in .cqs/ file listings
- **Difficulty:** easy
- **Location:** SECURITY.md:71 (Read Access table), SECURITY.md:84 (Write Access table)
- **Description:** Both filesystem access tables list `.cqs/index.hnsw.*` but omit `.cqs/splade.index.bin`, the third persisted index file introduced by #895. `src/splade/index.rs:35` defines `SPLADE_INDEX_FILENAME = "splade.index.bin"` and `src/cli/store.rs:168-175` documents it as living "alongside the HNSW files". SECURITY.md now under-reports the files cqs touches.
- **Suggested fix:** Add a `.cqs/splade.index.bin` row to both tables ("SPLADE sparse inverted index" / "Search operations" for read, "cqs index" for write).

## README `.cqs.toml` schema reference is two versions behind
- **Difficulty:** easy
- **Location:** README.md:35
- **Description:** Install section says `"current schema: v16"` but `src/store/helpers/mod.rs:68` is `pub const CURRENT_SCHEMA_VERSION: i32 = 18;`. Migrations v16→v17 (sparse_vectors, enrichment_version) and v17→v18 (embedding_base column) both exist in `src/store/migrations.rs:72-74`. The README claim was correct at v1.17 release but has not been touched for two schema bumps.
- **Suggested fix:** Change `"current schema: v16"` to `"current schema: v18"`.

## README env var table missing 8 CQS_* variables that exist in code
- **Difficulty:** easy
- **Location:** README.md:646-690 (Environment Variables table)
- **Description:** Grepping `src/` for `CQS_[A-Z_]+` produces 51 unique env vars; the README table lists 43. Missing: `CQS_DISABLE_BASE_INDEX` (`src/cli/store.rs:307`, v1.22.0 dual-HNSW eval bypass), `CQS_SKIP_INTEGRITY_CHECK` (`src/store/mod.rs:430`, shipped this session in #893), `CQS_SPLADE_MODEL`, `CQS_SPLADE_BATCH`, `CQS_SPLADE_MAX_SEQ`, `CQS_SPLADE_RESET_EVERY` (all in `src/splade/mod.rs`, required to use SPLADE-Code 0.6B), and `CQS_TYPE_BOOST`. `CQS_EVAL_TIMEOUT_SECS` is new in `evals/run_ablation.py` and not strictly a runtime var for cqs itself, but the rest are all read by the binary. This is a continuation of SHL-24/SHL-25.
- **Suggested fix:** Add a row per missing var with its default and description. At minimum, document `CQS_SKIP_INTEGRITY_CHECK` and `CQS_DISABLE_BASE_INDEX` since they materially affect safety/semantics.

## README CQS_WATCH_MAX_PENDING default wrong: 1000 vs actual 10_000
- **Difficulty:** easy
- **Location:** README.md:689 vs src/cli/watch.rs:57
- **Description:** Env var table lists `| CQS_WATCH_MAX_PENDING | 1000 | Max pending file changes before watch forces flush |`, but `max_pending_files()` falls back to `.unwrap_or(10_000)`. An agent that wants to reason about watch memory bounds sees a wrong number 10x too low. Code comment at `src/cli/watch.rs:49-50` says `"Maximum pending files to prevent unbounded memory growth. Override with CQS_WATCH_MAX_PENDING env var."` without a number — safe, but README cannot be trusted.
- **Suggested fix:** Change README to `10000` (match style of other `*_MAX_NODES` rows that already use `10000`).

## CONTRIBUTING.md Architecture Overview missing src/splade/
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:117-283 (Architecture Overview block)
- **Description:** The tree lists every top-level source directory except `src/splade/`. `src/splade/mod.rs` (`SpladeEncoder`, sparse vector type) and `src/splade/index.rs` (`SpladeIndex` with the new persistence format from #895) both exist and are referenced by production code paths (`src/cli/store.rs:168`, `src/cli/batch/mod.rs:275`). CLAUDE.md at lines 219-227 explicitly says CONTRIBUTING's overview must stay in sync with source file additions.
- **Suggested fix:** Add a `splade/` block next to `hnsw/` — e.g. `splade/ — SPLADE sparse encoder + persisted inverted index (v1.17+, index persistence v1.22.0)\n  mod.rs — SpladeEncoder, SparseVector type, encode()/encode_batch()\n  index.rs — SpladeIndex with persist/load (splade.index.bin + metadata.splade_generation invalidation)`.

## CONTRIBUTING.md Architecture Overview missing src/search/router.rs
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:189-194 (search/ block)
- **Description:** Lists `search/`: `mod.rs`, `scoring/`, `query.rs`, `synonyms.rs`. Missing `router.rs`, which is the v1.22.0 adaptive-retrieval query classifier (`QueryCategory`, `SearchStrategy`, `classify_query`). CHANGELOG calls this out as a 1.22.0 headline Added feature but CONTRIBUTING never received the corresponding module entry.
- **Suggested fix:** Add `router.rs — Query classifier (QueryCategory + SearchStrategy), adaptive routing for identifier/structural/behavioral/conceptual/multi-step/negation/type-filtered/cross-language intents` to the search/ block.

## CONTRIBUTING.md Architecture Overview missing src/store/sparse.rs
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:161-173 (store/ block)
- **Description:** Lists `store/`: `mod.rs, metadata.rs, search.rs, chunks/, notes.rs, calls/, types.rs, helpers/, migrations.rs`. Missing `sparse.rs`, which holds `Store::upsert_sparse_vectors`, `prune_orphan_sparse_vectors`, and the read paths that feed `SpladeIndex::build`. This file has been present since v1.17 (sparse_vectors table) and was touched by DS-1/DS-6 and EH-16 in v1.20.0 audit — its absence from the overview has been carried forward silently.
- **Suggested fix:** Add `sparse.rs — Sparse vector CRUD (SPLADE), upsert_sparse_vectors, prune_orphan_sparse_vectors, idx_sparse_token drop/recreate bulk pattern` to the store/ block.

## Stale doc comment references "in search.rs" for functions now in src/search/query.rs
- **Difficulty:** easy
- **Location:** src/store/search.rs:51, 55
- **Description:** Doc comment on `search_fts` says `"search_filtered (in search.rs)"` and `"search_filtered_with_index (in search.rs)"`, pointing a reader at the bare `search.rs` filename. The search module was split in v0.9.0 — `search_filtered` is now at `src/search/query.rs:116` and `search_filtered_with_index` is at `src/search/query.rs:542`. `search.rs` is ambiguous (both `src/store/search.rs` and `src/search/query.rs` could qualify) and neither matches the path a caller needs. An agent reading this doc to locate the function will waste a tool call.
- **Suggested fix:** Change both "(in search.rs)" references to "(in src/search/query.rs)" and keep the rest of the description.

## PRIVACY.md telemetry description missing persistence-by-file-presence
- **Difficulty:** easy
- **Location:** PRIVACY.md:7
- **Description:** Says `"Optional local-only command logging when CQS_TELEMETRY=1 is set. Stored in .cqs/telemetry.jsonl, never transmitted"`. Per `src/cli/telemetry.rs:37-40` and SECURITY.md:33, telemetry is also active if `.cqs/telemetry.jsonl` already exists, even without the env var. This means a user can opt in once (`cqs telemetry reset`) and telemetry persists across shells/subprocesses. Omitting this from PRIVACY.md understates the opt-in semantics and makes "unset env var = definitely no logging" technically false.
- **Suggested fix:** Update line 7 to: `"Optional local-only command logging when CQS_TELEMETRY=1 is set OR when .cqs/telemetry.jsonl already exists (persists opt-in across shells/subprocesses). Stored in .cqs/telemetry.jsonl, never transmitted. Delete the file and unset the env var to opt out."`

---

# API Design — v1.22.0 audit

14 findings. Most are inconsistencies between CLI and batch modes, or between subcommand families that handle the same concept differently. A few are new in v1.22.0 (SpladeIndex error shape, ensure/borrow two-phase API); the rest are pre-existing patterns that accumulated across the command surface.

## `--format` flag defined but ignored on 25+ commands

- **Difficulty:** medium
- **Location:** src/cli/dispatch.rs:172-315 (multiple cases), src/cli/definitions.rs:76-95 (`TextJsonArgs::effective_format`)
- **Description:** `TextJsonArgs` (the shared flatten struct for text/json commands) defines both `--format <text|json>` and `--json` as a shorthand, with `effective_format()` resolving `--json` as an override. Only 4 dispatch arms use `effective_format()`: `Impact`, `Review`, `Ci`, `Trace`. Every other command (`Blame`, `Brief`, `Stats`, `Deps`, `Callers`, `Callees`, `Neighbors`, `Explain`, `Similar`, `TestMap`, `Context`, `Dead`, `Gather`, `Affected`, `ImpactDiff`, `Diff`, `Drift`, `Health`, `Stale`, `Suggest`, `Read`, `Reconstruct`, `Related`, `Where`, `Scout`, `Plan`, `Task`, `AuditMode`, `Telemetry`, `Gc`) passes `output.json` directly, meaning `--format json` is silently accepted and ignored. A user who does `cqs stats --format json` gets text output with no warning. The flag shows up in `--help` on every command but only works on 4 of them.
- **Suggested fix:** Either (a) replace all `output.json` dispatch arms with `matches!(output.effective_format(), OutputFormat::Json)`, or (b) drop `--format` from `TextJsonArgs` entirely and only expose `--json`. Option (a) honors the documented contract; option (b) is simpler but kills the `--format mermaid` capability that `OutputArgs` (not `TextJsonArgs`) genuinely uses.

## Subcommand enums define inline `json: bool` instead of `TextJsonArgs`

- **Difficulty:** easy
- **Location:** src/cli/commands/infra/cache_cmd.rs:11-32 (CacheCommand), src/cli/commands/io/notes.rs:50-63 (NotesCommand::List), src/cli/commands/infra/project.rs:52-64 (ProjectCommand::Search), src/cli/commands/infra/reference.rs:53-57 (RefCommand::List)
- **Description:** AD-49 (v1.12.0) consolidated output-format args into `TextJsonArgs`/`OutputArgs`, which every top-level command uses. Every *subcommand enum* (`CacheCommand::Stats/Clear/Prune`, `NotesCommand::List`, `ProjectCommand::Search`, `RefCommand::List`) still defines inline `#[arg(long)] json: bool`. The result: `cqs stats --json` and `cqs notes list --json` look identical to the user but the second one doesn't go through `TextJsonArgs`, so if `TextJsonArgs` ever gains a `--format` override or JSON-pretty flag, half the surface area silently diverges. Same applies when someone adds `--pretty`, `--compact`, or `--yaml` to `TextJsonArgs`.
- **Suggested fix:** Flatten `TextJsonArgs` into each subcommand variant that has `json: bool` today. Mechanical change — unpack `output.json` in the handlers the same way top-level commands do.

## `--expand` has two mutually incompatible meanings across commands

- **Difficulty:** easy
- **Location:** src/cli/definitions.rs:219 (Cli.expand: bool), src/cli/args.rs:14-16 (GatherArgs.expand: usize)
- **Description:** The top-level `Cli` struct defines `--expand` as a `bool` meaning "expand results with parent context (small-to-big retrieval)". `GatherArgs` defines `--expand` as a `usize` meaning "call graph expansion depth (0=seeds only, max 5)". Both are flattened onto commands; a user running `cqs search foo --expand` gets parent-context expansion, while `cqs gather foo --expand 2` gets a BFS depth of 2. Worse: `cqs search foo --expand 2` *rejects* the value because the search expand is bool. The two meanings are completely unrelated operations (chunk assembly vs graph BFS) sharing the same flag name. Agents building a command pattern library will hit this the first time they try to parameterize gather's expansion after learning search's expand.
- **Suggested fix:** Rename one. Gather's expansion depth is the graph-BFS concept used by `impact --depth`, `test-map --depth`, `onboard --depth`; renaming `GatherArgs.expand` → `GatherArgs.depth` aligns it with the other graph commands and frees `--expand` for small-to-big retrieval everywhere. Alternatively, rename search's bool `--expand` → `--with-parents` (closer to the actual semantics).

## `blame --depth`/`-d` collides with graph-depth flags despite having no graph semantics

- **Difficulty:** easy
- **Location:** src/cli/args.rs:105-114 (BlameArgs)
- **Description:** `BlameArgs` defines `#[arg(short = 'd', long, default_value = "10")] pub depth: usize` where the doc says "Max commits to show". `--depth` is already the name used by `Impact`, `TestMap`, and `Onboard` for graph traversal depth; and `-d` is used by `Onboard` for "Callee expansion depth". An agent who has learned `impact foo --depth 5 = 5 transitive levels` will read `blame foo --depth 5` as the same thing, but it means 5 commits. AD-31 (v1.5.0) raised this as a `-n` overload; the current fix renamed `-n` → `-d` and kept the `depth` parameter name, which re-created the collision against other `--depth` flags. The AD-31 suggestion was `--limit`/`-n` — that got partially ignored.
- **Suggested fix:** Finish AD-31: rename `BlameArgs::depth` → `BlameArgs::limit`, short `-n`, update help to "Max commits to show". Reserve `--depth`/`-d` for graph traversal depth across the whole CLI.

## Batch `search` handler silently drops CLI-parity flags

- **Difficulty:** easy
- **Location:** src/cli/batch/handlers/search.rs:56-57, src/cli/batch/commands.rs:82-92 (BatchCmd::Search), src/cli/definitions.rs:209-220 (top-level Cli search flags)
- **Description:** The batch `search` command accepts `--context <N>`, `--expand`, and `--no-stale-check` to match CLI shape, then drops them on the floor: `let _ = (params.context, params.expand, params.no_stale_check);` with a comment "Accepted for CLI parity; batch JSON doesn't use line-context or parent expansion yet". Agents driving batch mode see `search --expand --context 3` succeed, get JSON back with no `context` lines and no parent expansion, and have no way to tell those flags were ignored. Additionally, batch `search` is missing four flags that top-level CLI *does* read: `--threshold`/`-t` (min similarity), `--pattern` (structural pattern filter), `--include-docs` (docs/md/config inclusion), `--semantic-only` (deprecated, but still accepted on CLI). The two surfaces have diverged asymmetrically — CLI has 4 flags batch doesn't, batch has 3 flags it silently ignores.
- **Suggested fix:** Either wire the three ignored flags through to the underlying search call (preferred — `context` and `expand` already work in CLI path; copy the logic), or reject them at parse time with a clap `conflicts_with = ...` or a pre-dispatch check. Add the missing `--threshold`, `--pattern`, `--include-docs` flags to `BatchCmd::Search` so divergence is visible and driven from a shared struct (the way `ImpactArgs`, `GatherArgs`, `ScoutArgs` are shared today).

## `Affected` missing `--stdin` while `ImpactDiff`/`Review`/`Ci` have it

- **Difficulty:** easy
- **Location:** src/cli/definitions.rs:320-327 (Affected), src/cli/definitions.rs:476-517 (ImpactDiff/Review/Ci)
- **Description:** Four commands operate on a git diff: `Affected`, `ImpactDiff`, `Review`, `Ci`. Three of them accept `--stdin` to read the diff from stdin; `Affected` only accepts `--base <ref>`. There's no technical reason `Affected` can't read stdin — `cmd_affected` calls the same `run_git_diff` helper and then parses hunks. The asymmetry forces pre-commit / CI pipelines to use different invocation styles across these four commands.
- **Suggested fix:** Add `#[arg(long)] stdin: bool` to `Commands::Affected` and pipe through to `cmd_affected` (wire the stdin-reading branch from `cmd_impact_diff` as a shared helper).

## `SpladeIndex::ensure_splade_index` + `borrow_splade_index` two-phase API

- **Difficulty:** medium
- **Location:** src/cli/batch/mod.rs:281-327, src/cli/store.rs:172-210 (CLI parallel `splade_index()`)
- **Description:** The CLI `CommandContext::splade_index(&self)` returns `Option<&SpladeIndex>` in a single call via `OnceLock`. The batch `BatchContext` exposes two methods the caller must invoke in order: `ensure_splade_index(&self)` (returns `()`, stashes the index in a `RefCell`) then `borrow_splade_index(&self)` (returns `Ref<Option<SpladeIndex>>`). Forgetting `ensure_` yields a `Ref` to `None` and silently degrades to cosine-only. Both methods log internally so there's no caller feedback: a bug in the wiring (e.g., missed `ensure_` in a new handler) is invisible in tests that use a store without sparse vectors. Two different APIs for the same operation across CLI and batch, plus the batch version leaks its internal `RefCell` concern into every call site.
- **Suggested fix:** Unify as `BatchContext::splade_index(&self) -> Option<Ref<SpladeIndex>>`. Internally: `ensure_` logic runs lazily on first call, then return `Some(Ref::map(self.splade_index.borrow(), |o| o.as_ref().unwrap()))`. Callers then use `if let Some(idx) = ctx.splade_index()` exactly like the CLI path. The two-phase split is not earning its cost.

## `SpladeIndexPersistError` wraps invariant violations as `Io(InvalidData)`

- **Difficulty:** easy
- **Location:** src/splade/index.rs:42-59 (definition), :226-256, :405-471 (construction sites)
- **Description:** The new error enum has dedicated variants for `BadMagic`, `UnsupportedVersion`, `GenerationMismatch`, `ChecksumMismatch`, and `Truncated` — clean. Every other failure mode goes through `Io(std::io::Error::new(InvalidData, format!(...)))`: chunk id exceeds `u32::MAX`, posting list count overflow, `chunk_idx` overflow, body `chunk_count` doesn't fit in `usize`, invalid utf-8 in chunk id, `posting chunk_idx` out-of-bounds. Seven distinct invariant violations all reported as `io::Error`. A caller pattern-matching on the error variant can only distinguish "IO failed" from "five specific named cases"; the more interesting internal corruption cases are unstructured strings inside an `io::Error`. Compare to `HnswError` (src/hnsw/mod.rs:105-125) which has `ChecksumMismatch { file, expected, actual }` with structured fields.
- **Suggested fix:** Add an `InvalidData(String)` or `Corruption(&'static str, String)` variant to `SpladeIndexPersistError` and move the seven string-based `io::Error::new(InvalidData, ...)` constructions to it. `BadMagic`, `Truncated`, and `ChecksumMismatch` stay. Only genuine I/O failures (file open, read, write) should remain `Io(#[from])`.

## `SpladeIndex::ChecksumMismatch` carries no details; `HnswError::ChecksumMismatch` does

- **Difficulty:** easy
- **Location:** src/splade/index.rs:55-56 (`ChecksumMismatch`), src/hnsw/mod.rs:117-124 (`ChecksumMismatch { file, expected, actual }`)
- **Description:** `HnswError::ChecksumMismatch` has structured `file`, `expected`, and `actual` fields so logs and error messages can identify *which* file and *what* the mismatch was. `SpladeIndexPersistError::ChecksumMismatch` is a unit variant with a fixed string "SPLADE index body checksum mismatch — file is corrupt". Two neighboring persistence modules built by the same author at different times with different error-detail conventions. When the SPLADE checksum fails, the operator has no information beyond "corrupt" — no path, no hashes to compare against backups.
- **Suggested fix:** Make `ChecksumMismatch` a struct variant with `path: String`, `expected: String`, `actual: String` (hex). Mirror the HNSW shape so errors across the two persistence layers look uniform.

## `--semantic-only` is defined on `Cli` but never read

- **Difficulty:** easy
- **Location:** src/cli/definitions.rs:181-183
- **Description:** `Cli::semantic_only: bool` has `#[arg(long)]` and a doc comment that says "deprecated — RRF now off by default". It appears in `cqs --help` so an agent reading the help text will assume it's live. A Grep across the codebase finds zero readers: `grep -n "semantic_only" src/` turns up only the definition site. Since RRF is no longer the default, `--semantic-only` has no effect — it's a dead flag still advertised to users.
- **Suggested fix:** Delete the field from `Cli`. No external users means no deprecation cycle needed (per project conventions). One line to delete plus one line of doc comment.

## `cmd_plan::display_plan_text` accepts `_tokens` and never uses it

- **Difficulty:** easy
- **Location:** src/cli/commands/train/plan.rs:37-74
- **Description:** `cmd_plan` accepts `--tokens <N>` from clap, then calls `display_plan_text(&result, root, tokens)` which has the parameter as `_tokens: Option<usize>`. The underscore prefix is Rust's "deliberately unused" marker. JSON mode respects the budget at line 26 by adding `"token_budget": N` to the JSON. Text mode accepts the flag and silently prints the full output. An agent who set `--tokens 500` expecting truncation in text output gets whatever the full plan's length is, with no warning. The CLI contract says `--tokens` applies waterfall budgeting across sections — this command breaks it silently.
- **Suggested fix:** Either (a) implement actual text-mode budgeting (truncate scout groups + checklist items to fit under `tokens`), or (b) add a `tracing::warn!` on the dispatch path if `tokens.is_some()` and text mode is chosen, or (c) reject `--tokens` + text mode with a clap `conflicts_with = "format"` pre-check. Option (a) is the "always do things properly" choice.

## `ProjectCommand::Search --limit` default 10 vs top-level `--limit` default 5

- **Difficulty:** easy
- **Location:** src/cli/commands/infra/project.rs:52-57, src/cli/definitions.rs:139 (top-level Cli.limit default 5)
- **Description:** `cqs <query>` (top-level search) defaults `--limit 5`. `cqs project search <query>` defaults `--limit 10`. Two commands whose surface looks identical return different numbers of results by default. The same applies less severely across `GatherArgs.limit = 10` (defensible — BFS expansion yields more candidates), `BlameArgs.depth = 10` (commits, unrelated concept), `Where.limit = 3` (defensible — fewer file suggestions are expected), but `project search` with 10 looks like an oversight — it's a "search across projects" that should match the single-project search default.
- **Suggested fix:** Change `ProjectCommand::Search::limit` default to 5 to match top-level search. If the 10 default was deliberate (e.g., because cross-project searches are noisier), add a comment explaining it.

## `TrainData::max_commits = 0` sentinel vs `TrainPairs::limit = Option<usize>`

- **Difficulty:** easy
- **Location:** src/cli/definitions.rs:725-743 (TrainData) vs :745-759 (TrainPairs)
- **Description:** Two neighboring commands express "unlimited" using opposite patterns. `TrainData` defines `max_commits: usize` with `default_value = "0"` and docs "(0 = unlimited)". `TrainPairs` defines `limit: Option<usize>` with no default, using `None` for unlimited. Both are the current file's style (same author, same feature area). A caller reading both has to remember which command uses 0-sentinel and which uses `Option::None`. `Drift::limit: Option<usize>` follows the TrainPairs pattern.
- **Suggested fix:** Change `TrainData::max_commits` to `Option<usize>` to match the rest of the codebase. `0` as a valid limit should be rejected at parse time (or treated as "zero commits, no-op") — right now `--max-commits 0` means "unlimited", which is surprising from the clap help text.

## `Store::upsert_sparse_vectors` and `prune_orphan_sparse_vectors` duplicate generation-bump logic

- **Difficulty:** medium
- **Location:** src/store/sparse.rs:126-144 (upsert bump), :243-258 (prune bump), :270-278 (getter)
- **Description:** The new `splade_generation` counter exists to invalidate persisted SPLADE index files. It has one getter (`splade_generation() -> Result<u64>`) and two writers, each of which inlines the same `SELECT value ... FROM metadata`, parse, saturating_add(1), `INSERT ... ON CONFLICT DO UPDATE` sequence — 12 lines duplicated, only the `&mut *tx` vs `&self.pool` executor differs. If the semantics change (e.g., pad with leading zeros, add a timestamp, guard against overflow), they will drift. The two writers aren't even symmetric — `prune_orphan_sparse_vectors` only bumps when `rows_affected > 0` (correct — no change means no invalidation), but `upsert_sparse_vectors` bumps unconditionally even for empty vectors arriving via a no-op path. An agent adding a third mutation site (e.g., a `delete_sparse_vectors_by_chunk` helper) will copy-paste the 12-line block or forget the bump entirely.
- **Suggested fix:** Add a private `Store::bump_splade_generation_tx(tx: &mut Transaction) -> Result<()>` helper (for use inside write transactions) and `Store::bump_splade_generation(&self) -> Result<()>` for pool-based writes. Both writers call into them. Expose nothing additional on the public API. While there: make the getter return `u64` not `Result<u64>` — the only caller in `cli/store.rs:175` treats `Err` as `0` with a warning, and the getter's only failure is "SQLite query failed" which is already a data-store-wide precondition.

---

# Error Handling — v1.22.0 audit

9 findings — most concentrate on the PR #895 SPLADE persistence path (silent generation reads + bare `unwrap_or` + parse-on-corrupt that can create cross-invocation poisoning), plus 3 older silent-fallback patterns and one `parse_server_code_calls`/`parse_server_code_types` inconsistency vs its sibling.

## EH-1: `splade_generation()` missing tracing span, silent parse failure on corrupt metadata

- **Difficulty:** easy
- **Location:** src/store/sparse.rs:270-278
- **Description:** `splade_generation()` has no `tracing::info_span!` at entry (the MEMORY.md rule requires one) and line 276 silently treats a non-parseable `splade_generation` metadata value as `0` via `s.parse::<u64>().ok().unwrap_or(0)`. A corrupt or manually-edited metadata value will collapse the counter to 0, after which the next `upsert_sparse_vectors` bumps the on-table value to `1`. If a previously-persisted `splade.index.bin` is also at generation 1 (from a prior natural bump), the staleness check passes and callers are served a structurally stale index. The same pattern repeats in `upsert_sparse_vectors` (line 130-137) and `prune_orphan_sparse_vectors` (line 244-251), so there is no single mitigation site.
- **Suggested fix:** Add `let _span = tracing::info_span!("splade_generation").entered();` at the top of the method. Replace the silent parse with a match that logs a `tracing::warn!` on parse failure, and return `StoreError::Runtime("corrupt splade_generation metadata")` so the caller treats the condition as a rebuild trigger rather than a silent reset. Apply the same `warn` + explicit match in the two bump sites in `upsert_sparse_vectors` and `prune_orphan_sparse_vectors` so a corrupt counter is never silently re-seeded.

## EH-2: `cmd_index` persists SPLADE index with `splade_generation().unwrap_or(0)`, no warn

- **Difficulty:** easy
- **Location:** src/cli/commands/index/build.rs:536
- **Description:** Immediately after encoding sparse vectors, the post-index persist path reads `let generation = store.splade_generation().unwrap_or(0);` with no tracing, no warn, no error propagation. If the metadata query fails on this specific path (pool saturation after heavy batch encode, locked WAL, transient I/O), the persist uses generation 0 and writes a file whose header records generation 0 — but `upsert_sparse_vectors` (line 513) has already bumped the on-disk generation counter to ≥1 before we got here. The next CLI invocation then reads generation N ≥ 1 from the DB, opens the file, sees header generation 0, returns `GenerationMismatch`, and unnecessarily rebuilds from SQLite at ~45 s. The fix requested by CLAUDE.md ("never bare `.unwrap_or_default()`") directly applies here. CommandContext::splade_index and BatchContext::ensure_splade_index already use the correct pattern — this is the only site that regressed.
- **Suggested fix:** Replace with the same match used at `src/cli/store.rs:175-181`: `let generation = match store.splade_generation() { Ok(g) => g, Err(e) => { tracing::warn!(error = %e, "Failed to read splade_generation for index persist, skipping SPLADE persist — next query will rebuild"); return; } };`. Skipping the persist on error is preferable to writing a wrong-generation file because the next load will transparently rebuild from SQLite and re-save the correct generation.

## EH-3: `load_or_build` persists with generation 0 when caller read failed, poisoning cache

- **Difficulty:** medium
- **Location:** src/splade/index.rs:500-534, callers src/cli/store.rs:172-210 and src/cli/batch/mod.rs:281-320
- **Description:** Both `CommandContext::splade_index` and `BatchContext::ensure_splade_index` catch a `splade_generation()` read failure and substitute `0` (lines 179 and 290). They then pass `0` to `load_or_build`. If `load_all_sparse_vectors()` subsequently succeeds (vectors are stored under a different table, so the two paths can fail independently on transient metadata-table errors), `load_or_build` builds a non-empty index and calls `save(path, 0)`. The persisted file's header now records generation 0 while the real on-disk counter is ≥1. Every future load mismatches and rebuilds, and every rebuild overwrites the file with generation 0 again. The "best-effort persist" (comment line 519) becomes a self-perpetuating cache poisoning loop with no log beyond the original one-time warn.
- **Suggested fix:** Introduce a sentinel so the persist step can opt out when the generation wasn't trustworthy. Options: (1) change the caller signature to pass `Option<u64>` and skip `save` when the generation is `None`; (2) add a separate `build_in_memory_only()` entry point for the degraded case; or (3) simplest, have both callers return `None` (no SPLADE) when `splade_generation()` fails instead of falling through with `0`. Option (3) is closest to `CommandContext::splade_index`'s contract ("`None` when the store contains no sparse vectors") and avoids writing garbage to disk.

## EH-4: `SpladeIndexPersistError::Io` overloaded to carry non-I/O corrupt-data conditions

- **Difficulty:** easy
- **Location:** src/splade/index.rs:227-260, 405-444, 463-472
- **Description:** `SpladeIndex::save` and `::load` use `SpladeIndexPersistError::Io(std::io::Error::new(InvalidData, ...))` for five distinct structural conditions: chunk id > u32::MAX bytes, posting list > u32::MAX entries, chunk_idx > u32::MAX, chunk_count doesn't fit usize, posting chunk_idx out of bounds for id_map. None of these are I/O errors. Folding them into `Io(std::io::Error)` makes the enum less expressive than the dedicated `Truncated(u64)`, `BadMagic`, `ChecksumMismatch`, `GenerationMismatch` variants already in the enum, and means callers matching on variants for metrics/recovery decisions cannot distinguish "disk read error" from "payload structurally invalid". The Display text ends up as `io: chunk id exceeds u32::MAX bytes: …` which is a nonsense prefix.
- **Suggested fix:** Add `#[error("corrupt SPLADE index payload: {0}")] CorruptData(String)` to `SpladeIndexPersistError` and route all five synthetic `InvalidData` sites through it. Keep `Io(#[from] std::io::Error)` for real I/O only.

## EH-5: `parse_server_code_calls` and `parse_server_code_types` silently return empty on all parser errors

- **Difficulty:** easy
- **Location:** src/parser/aspx.rs:303-355 (calls) and 425-478 (types)
- **Description:** Both functions short-circuit with `return vec![]` on five failure points: `language.try_def()` returns None, `set_language` fails, `set_included_ranges` fails, `ts_parser.parse` returns None, or `get_query` fails — none of which emit any log. The sibling function `parse_server_code` at lines 194-259 uses the identical flow but logs every one of those conditions with `tracing::warn!`. The silent variants mean that if an ASPX file has a language declaration that cqs can't parse (e.g., a grammar feature disabled in this build), the chunk stream still gets populated by `parse_server_code` but the call graph and type-edge streams silently lose every call and type reference for that file, and the operator has no indication that type/call data is missing for ASPX server code.
- **Suggested fix:** Copy the `tracing::warn!` calls from `parse_server_code` (lines 225-258) into the same positions in `parse_server_code_calls` and `parse_server_code_types`. All five sites should log with `%language` and the failure reason before returning `vec![]`.

## EH-6: `EmbeddingBatchIterator::next` silently drops rows with corrupt embedding blobs

- **Difficulty:** easy
- **Location:** src/store/chunks/async_helpers.rs:438-441
- **Description:** The batch iterator drives full-corpus HNSW builds. Per-row embedding decoding is `bytes_to_embedding(&bytes, self.store.dim).ok().map(...)`. If a row's blob has the wrong byte length — which means a `EmbeddingBlobMismatch` that was explicitly designed ("This prevents silently using corrupted/truncated embeddings" — comment at src/store/helpers/embeddings.rs:47) — the row is filtered out of the batch with no log, no counter, no warn. The HNSW build completes with a smaller index than the store reports in `chunk_count`, and the only visible evidence is a silent drop in `vectors` count when the operator inspects the build output. Worst case: a schema migration bug that writes wrong-dim blobs for a subset of rows → HNSW is missing those chunks for the rest of the index's life, until the next rebuild catches it.
- **Suggested fix:** Replace `.ok()` with an explicit match that counts and logs: `match bytes_to_embedding(&bytes, self.store.dim) { Ok(emb) => Some((id, Embedding::new(emb))), Err(e) => { tracing::warn!(chunk_id = %id, error = %e, "Skipping chunk with corrupt embedding blob during HNSW build"); None } }`. Consider returning the drop count via `IndexStats` or a new `hnsw_build_dropped` metric so the operator sees it on the summary line.

## EH-7: Watch-mode HNSW load failure indistinguishable from "first run"

- **Difficulty:** easy
- **Location:** src/cli/watch.rs:307-314
- **Description:** On watch startup, `HnswIndex::load_with_dim(...)` is matched with `Err(_) => (None, 0)` — a failed load is silently treated the same as "no prior index exists". `HnswError` has distinct variants for `NotFound`, `DimensionMismatch`, `Build error`, and IO — a `NotFound` is valid "first run", but `DimensionMismatch` or IO failure means the operator's on-disk index is unusable and watch is about to silently rebuild from scratch. The user sees no indication that the previous index was discarded or why. This is the same class as EH-14 in the v1.20.0 audit (silently ignoring DB errors during index init).
- **Suggested fix:** Match on error type: if `HnswError::NotFound`, log at `debug` ("no prior HNSW, starting fresh"); on any other error, log at `warn` with the error, so operators see "existing HNSW unusable, rebuilding" and can correlate with the underlying cause.

## EH-8: `check_index_staleness` silently returns on stat failure

- **Difficulty:** easy
- **Location:** src/cli/batch/mod.rs:140-145
- **Description:** The batch-mode cache-invalidation check opens `index.db` via `std::fs::metadata().and_then(|m| m.modified())` and on any error runs `Err(_) => return` without logging. If the DB file becomes temporarily unstattable (permissions churn, ENOENT during a concurrent rebuild, network filesystem glitch), every subsequent command in the batch session keeps using stale caches forever — the mtime was never recorded, so `last != Some(current_mtime)` never fires even after the file comes back. Batch sessions can be long-lived (days) and operators have no way to tell the cache is stuck.
- **Suggested fix:** Log at `tracing::warn!` on the first stat failure ("Cannot stat index.db for batch staleness check, caches may remain stale"). Also consider setting `self.index_mtime.set(None)` on failure so a subsequent successful stat triggers the invalidation path.

## EH-9: `Drop for Store` discards `catch_unwind` panic payload silently

- **Difficulty:** easy
- **Location:** src/store/mod.rs:621-629
- **Description:** The WAL checkpoint on drop is wrapped in `std::panic::catch_unwind` to handle the "block_on inside async runtime" edge case. The outer `let _ = std::panic::catch_unwind(...)` swallows any panic payload. When the inner `block_on` does panic (e.g., the edge case the `catch_unwind` exists to handle), nothing is logged — operators can't tell a panic was caught vs. a clean drop. In Drop paths you can't propagate, but you can log.
- **Suggested fix:** Replace `let _ =` with `if let Err(payload) = std::panic::catch_unwind(...)` and log at `tracing::warn!` with the panic message extracted from the payload (`payload.downcast_ref::<&str>()` / `String`), so the crash is at least visible in telemetry.

---

# Observability — v1.22.0 audit

Ten findings in post-v1.21.0 code, concentrated in: silent SPLADE fallback when index is empty, missing spans on the top-level CLI dispatch and `begin_write`, silent env-var bypass on the integrity check, a new telemetry path with zero error visibility, stale `{}` format-string log calls in `cagra.rs`, and corrupt-embedding skips logged at `trace!` level (invisible at default).

## OB-13: `search_hybrid` silently falls back when `splade_index` is empty

- **Difficulty:** easy
- **Location:** src/search/query.rs:407-409 (paired with src/cli/store.rs:197-200)
- **Description:** The v1.20.0 OB-7 fix promoted the "SPLADE model not found" log to `warn!` in `CommandContext::splade_encoder`. But `search_hybrid` still silently delegates to `search_filtered_with_index` whenever `splade.is_none()`. The second arm — encoder exists and query encoded fine, but `splade_index()` returned `None` because the store has zero sparse vectors — hits the `tracing::debug!("No sparse vectors in store, SPLADE index unavailable")` in `cli/store.rs:198` (invisible at default `warn` level), then `ctx.splade_index` is `None` in `cmd_query_project`, and `search_hybrid` at line 407 returns to dense-only with no log. A user who indexed before enabling SPLADE, or who deleted `sparse_vectors`, runs `--splade` and gets dense-only results with zero indication. No warn at any level.
- **Suggested fix:** Promote `cli/store.rs:198` from `debug!` to `warn!` when the getter is invoked (first call). Alternatively add `tracing::warn!("--splade requested but sparse_vectors empty, falling back to dense-only. Run 'cqs index --force' after enabling SPLADE.")` at `search/query.rs:407` when `filter.enable_splade && splade.is_none()`. The second location is more defensive because it also catches the SPLADE query-encoding failure path.

## OB-14: `cli::run_with` (top-level CLI dispatch) has no root tracing span

- **Difficulty:** easy
- **Location:** src/cli/dispatch.rs:23 (`pub fn run_with`)
- **Description:** Every `cqs` invocation flows through `run_with`, but it has no `info_span!` or `debug_span!`. `cmd_*` handlers have their own spans but they are orphaned — no parent span means no correlation id, no way to group log entries for one command invocation when `RUST_LOG=info` is set, and no total command timing. This is the CLI analog of the OB-5 fix that added `batch_dispatch` span to `cli/batch/commands.rs:387`. The main CLI path is still unrooted.
- **Suggested fix:** Add `let _span = tracing::info_span!("cqs", cmd = ?cli.command, verbose = cli.verbose).entered();` at the top of `run_with` before telemetry logging. Use the command discriminant as a static field so all per-command logs inherit "cqs.cmd=index" etc.

## OB-15: PRAGMA quick_check path has no tracing, silent env-var bypass

- **Difficulty:** easy
- **Location:** src/store/mod.rs:430-441
- **Description:** PR #893 downgraded `PRAGMA integrity_check` → `quick_check` on write opens and added `CQS_SKIP_INTEGRITY_CHECK=1` to skip entirely. But the integrity check body has no `tracing::debug!` entry/exit, no elapsed-time log, and no log when `skip_integrity` short-circuits or when `config.read_only` causes the check to be skipped. An operator investigating a startup stall has no way to tell from logs whether the check ran (and took 5s) vs. was bypassed. sqlx slow-statement logging catches the actual PRAGMA at the 5s threshold, so timing is partially covered, but the **bypass paths** (env var, read-only) are completely silent. That matters because if a user sets `CQS_SKIP_INTEGRITY_CHECK=1` in `~/.bashrc` and forgets, every session silently skips the canary.
- **Suggested fix:** Add one log line at each path: `tracing::debug!("PRAGMA quick_check skipped (read-only open)")` when `config.read_only`, `tracing::warn!("PRAGMA quick_check skipped by CQS_SKIP_INTEGRITY_CHECK=1")` when the env var short-circuits, and wrap the existing block in a `tracing::debug_span!("store_quick_check", path = %path.display()).entered();` so slow-statement logs inherit the span context.

## OB-16: `telemetry::log_routed` silently swallows write failures

- **Difficulty:** easy
- **Location:** src/cli/telemetry.rs:136-141
- **Description:** The new `log_routed` helper (added with adaptive retrieval PR #873) uses `let _ = (|| -> io::Result { ... })();` to silently drop IO errors. `log_command` at line 99 uses the same pattern but at least has tracing on the auto-archive path. `log_routed` has **zero** tracing on any failure. If `.cqs/telemetry.jsonl` is unwritable (disk full, permission error, parent dir missing), every routed search silently loses its telemetry entry. Unlike `log_command`, it also does not acquire the advisory `telemetry.lock` — so a concurrent `cqs telemetry reset` can interleave writes. The silence means adaptive-routing eval runs lose data with no warning.
- **Suggested fix:** Either factor out a shared internal writer used by both `log_command` and `log_routed` so the lock + error-handling logic lives in one place, or at minimum add `.or_else(|e| { tracing::debug!(error = %e, "log_routed write failed"); Ok(()) })` to the closure. Debug level is appropriate since telemetry is best-effort.

## OB-17: `splade_generation()` silently collapses unparseable value to 0

- **Difficulty:** easy
- **Location:** src/store/sparse.rs:270-278
- **Description:** `row.and_then(|(s,)| s.parse::<u64>().ok()).unwrap_or(0)` at line 276 returns 0 for three distinct conditions: row missing (fresh v17), row present but not-a-number (corruption/manual edit), row present and empty. All three look the same to the caller. `build.rs:536` then calls `store.splade_generation().unwrap_or(0)` on top of this, flattening a StoreError to 0 too. An on-disk SpladeIndex persisted at generation=0 will fail every future load-check when the real generation advances, forcing silent rebuilds at query time. Contrast with `store/mod.rs:461-467` which emits `tracing::warn!(raw = %s, "dimensions metadata is 0 — invalid, using default")` for the exact same pattern on the dimensions row.
- **Suggested fix:** Match the dimensions pattern. Replace the `and_then().ok()` with an explicit match that emits `tracing::warn!(raw = %s, "splade_generation metadata is not a valid u64, using 0")` on parse failure. Also change `build.rs:536` to handle the Err branch with a warn instead of silently collapsing via `unwrap_or(0)`.

## OB-18: `cagra.rs` uses format-string logging throughout (14 call sites)

- **Difficulty:** easy
- **Location:** src/cagra.rs:93, 146, 205, 226, 240, 261, 275, 289, 313, 319, 323, 443, 507 (13 call sites) plus one `{} dims` at 240
- **Description:** Every log call in `cagra.rs` uses `tracing::info!("... {}", var)` or `tracing::error!("... {}", e)` instead of structured `tracing::error!(error = %e, "...")` fields. This means `RUST_LOG_FORMAT=json` emitters cannot index the error field, grep/filter on `n_vectors=` won't match, and structured log consumers lose all discriminators. This is the only file in the codebase with this many non-structured log calls clustered together — every other post-v0.12.1 module uses field syntax. The same anti-pattern exists in `hnsw/persist.rs:191,535,652`, `hnsw/build.rs:71,228`, `reference.rs:150`, and `search/query.rs:589,593` but at lower density (1-3 per file).
- **Suggested fix:** Mechanical rewrite of all 14 `cagra.rs` sites: `tracing::info!("Building CAGRA index with {} vectors", n)` → `tracing::info!(vectors = n, "Building CAGRA index")`. For errors: `tracing::error!("Failed to rebuild CAGRA index: {}", e)` → `tracing::error!(error = %e, "Failed to rebuild CAGRA index")`. Same treatment for the other 8 sites in `hnsw/`, `reference.rs`, and `search/query.rs:589,593`.

## OB-19: Corrupt embedding skips logged at `trace!` level (invisible at default)

- **Difficulty:** easy
- **Location:** src/store/chunks/embeddings.rs:52, src/store/chunks/query.rs:347
- **Description:** Both embedding-lookup functions handle blob-decode failures via `tracing::trace!(hash = %hash, error = %e, "Skipping embedding")`. `trace!` is below `debug!` — invisible unless `RUST_LOG=trace` is set. A corrupt embedding blob (wrong dim, truncated, bit-rot) is dropped silently, the caller sees a cache miss instead of a corruption signal. Corrupt entries linger indefinitely because nothing ever flags them for cleanup. Compare `chunks/query.rs:280` which uses `tracing::warn!(chunk_id = %row.id, error = %e, "Corrupt embedding for chunk, skipping")` for the identical case in `get_chunk_with_embedding`. The pattern is inconsistent within the same file.
- **Suggested fix:** Promote both `trace!` calls to `warn!` and unify on the `get_chunk_with_embedding` message text. Corrupt embeddings are data-safety events, not ambient noise — they should be visible at the default log level so users know to run `cqs index --force` to clean them up.

## OB-20: `Store::begin_write` has no span, write-lock contention invisible

- **Difficulty:** easy
- **Location:** src/store/mod.rs:506-518
- **Description:** `begin_write` acquires the global `WRITE_LOCK` mutex (DS-5 fix) and then begins a sqlx transaction. There is no `info_span!` or `debug_span!` wrapping either step. If two in-process writers contend (e.g., `cqs index` racing with a stray `cqs notes add`, or the new sparse-vectors path overlapping with llm_summaries), the blocked thread waits on `WRITE_LOCK.lock()` with zero observability — no "waited 300ms for write lock" log, no span timing. sqlx slow-statement logging catches only the transaction itself, not the lock wait before it. Post-hoc investigation of a "why did index take 90s?" question is impossible without adding ad-hoc prints.
- **Suggested fix:** Wrap the body in `let _span = tracing::debug_span!("begin_write").entered();` and emit `tracing::debug!(wait_us = ?elapsed, "Acquired WRITE_LOCK")` after `guard = WRITE_LOCK.lock()` if `elapsed > 10ms`. A `trace!` on fast acquisition keeps the log clean; a `debug!` on slow acquisition surfaces contention. For production debugging, an opt-in `warn!` via env var would also be reasonable.

## OB-21: `BoundedScoreHeap::push` non-finite score warn has no context fields

- **Difficulty:** easy
- **Location:** src/search/scoring/candidate.rs:172
- **Description:** `tracing::warn!("BoundedScoreHeap: ignoring non-finite score")` is emitted with no structured fields: no chunk id, no raw score value, no query context. When this fires (which it does on NaN scores from malformed embeddings, broken FTS ranks, or cosine NaN from zero-norm query vectors), the operator sees only "something was non-finite somewhere in search". Impossible to trace back to the offending chunk or reproduce the bug without adding ad-hoc prints. The warn is already the right level — the message just needs the usual structured fields.
- **Suggested fix:** `tracing::warn!(id = %id, score = ?score, "BoundedScoreHeap: ignoring non-finite score")`. `?score` (Debug) prints `NaN` or `inf` as the literal token so grep-filtering works.

## OB-22: Watch mode never updates sparse_vectors, no log when SPLADE drifts out of sync

- **Difficulty:** medium
- **Location:** src/cli/watch.rs:684 (`reindex_files`) — no SPLADE handling anywhere in the file
- **Description:** `reindex_files` in watch mode re-parses, re-embeds, and inserts chunks via `store.insert_chunks` / HNSW update, but it does **not** call `upsert_sparse_vectors`, does not run SPLADE encoding, and does not bump `splade_generation`. A user running `cqs watch` who edits a file sees HNSW stay in sync while the SPLADE inverted index silently drifts. On the next `--splade` query, hybrid search returns results against stale sparse vectors for just-edited files (content_hash moved, sparse entries still point at the old chunk_id that was replaced). Not only is the feature broken in watch mode — there is no log at all warning the user that watch skipped the sparse step. The user only discovers this when `cqs search --splade` returns wrong results.
- **Suggested fix:** Two parts. (1) **Fix the gap**: after the HNSW update in `reindex_files`, if `sparse_vectors` contains entries for the new chunks' content hashes, run SPLADE encoding on the changed files only and call `upsert_sparse_vectors`. (2) **Observability**: if the SPLADE encoder is unavailable in watch mode, emit `tracing::warn!("Watch mode cannot refresh SPLADE — run 'cqs index --force' to rebuild sparse_vectors after this session")` once per cycle when sparse vectors exist but can't be updated. This is medium because the fix requires wiring the encoder through the watch context, not just adding a log line.

---

# Test Coverage (Adversarial) — v1.22.0 audit

10 adversarial test gaps across SPLADE persistence, sparse store, read-only integrity, embedding NaN propagation, and concurrent writers.

## [Missing test: SpladeIndex::load — attacker-tampered chunk_count triggers unbounded allocation]
- **Difficulty:** easy
- **Location:** src/splade/index.rs:389-411 (header parse + Vec::with_capacity before body walk)
- **Description:** The body checksum (blake3) covers only the body bytes; `chunk_count` and `token_count` live in the header and are NOT hashed. An attacker (or fs corruption) can rewrite a valid file's header to set `chunk_count = u32::MAX`, and the checksum still passes. `Vec::with_capacity(chunk_count_usize)` at line 411 then attempts ~96GB on 64-bit, aborting the process. Same issue for `token_count_usize` at line 445 → `HashMap::with_capacity`. `test_persist_corrupt_body_rejected` only covers a body-byte flip.
- **Suggested fix:** Add `test_persist_header_chunk_count_inflated_rejected` — build a real file, rewrite bytes [16..24] to a very large value, assert `load` returns `Err` (Truncated) BEFORE the capacity allocation, OR add a sanity cap on `chunk_count`/`token_count` (e.g., 100M) before allocating and test both the accept and reject cases.

## [Missing test: SpladeIndex::load — chunk_count_usize lies, body is legitimate but claims N+1 chunks]
- **Difficulty:** easy
- **Location:** src/splade/index.rs:422-437 (id_map loop driven by header count)
- **Description:** A header claiming `chunk_count = actual + 1` passes hash check if header is not covered; currently the loop invokes `need(&body, cursor, 4)` which catches it as Truncated — BUT only after `Vec::with_capacity(chunk_count_usize)` already allocates for the bad count. No test verifies the Truncated path fires when chunk_count exceeds the body. The Truncated error enum exists but has zero tests.
- **Suggested fix:** Add `test_persist_truncated_body_returns_truncated` that writes a valid header + short body (fewer id_map bytes than the header claims) and asserts `SpladeIndexPersistError::Truncated` is returned.

## [Missing test: SpladeIndex::load — non-UTF8 chunk id bytes]
- **Difficulty:** easy
- **Location:** src/splade/index.rs:427-434 (`std::str::from_utf8` path)
- **Description:** The parser rejects non-UTF8 chunk IDs but no test exercises this branch. `test_persist_corrupt_body_rejected` flips one byte in the id-length prefix, which lands in ChecksumMismatch, not Utf8. An attacker-written file with a valid checksum but non-UTF8 id bytes is untested.
- **Suggested fix:** Add `test_persist_invalid_utf8_chunk_id_rejected` — build id_map + postings with a known chunk_id, hand-construct a file whose body replaces the valid UTF-8 bytes with `[0xFF, 0xFE, 0xFD]` (same length so chunk_count still works), recompute the blake3 of the new body, rewrite the header with the new hash, and assert `load` returns Err (InvalidData, "not valid utf-8").

## [Missing test: SpladeIndex::load — posting_count lies, saturating_mul overflows]
- **Difficulty:** easy
- **Location:** src/splade/index.rs:453-456 (`posting_count.saturating_mul(8)`)
- **Description:** A tampered posting_count of `u32::MAX` (→ 4G postings) saturates to `usize::MAX` in the `need()` check, correctly returning Truncated, but only AFTER `Vec::with_capacity(posting_count)` tries to allocate 4G*(usize+f32)=48GB. No test covers this path.
- **Suggested fix:** Add `test_persist_posting_count_inflated_triggers_truncated` that builds a valid file then rewrites a posting_count u32 LE to `u32::MAX` in-place (within the hashed body so checksum is rebuilt). Assert the load fails cleanly without OOM (you'll need a cap on posting_count).

## [Missing test: Store::open_with_config — read-only opens actually skip integrity check]
- **Difficulty:** easy
- **Location:** src/store/mod.rs:411-441 (quick_check gate on `!config.read_only && !skip_integrity`)
- **Description:** PR #893 shipped with zero test changes. Nothing verifies that `open_readonly*` actually bypasses the quick_check, nor that write opens still run it, nor that `CQS_SKIP_INTEGRITY_CHECK=1` bypasses on write. A future refactor can silently re-enable the 85s walk on read-only opens and break the eval harness invisibly. Also no test that `PRAGMA quick_check` returning a non-"ok" value on a write open raises `StoreError::Corruption`.
- **Suggested fix:** Three tests — (1) `test_readonly_open_skips_integrity_check` patching a file to intentional-corruption shape and asserting `open_readonly` succeeds where `open` would fail; (2) `test_write_open_runs_quick_check_and_fails_on_corruption` using a corrupted page to force a non-ok quick_check; (3) `test_cqs_skip_integrity_check_env_bypasses_on_write` setting the env var, asserting write-open of a "corrupt" DB succeeds.

## [Missing test: Store::get_embeddings_by_hashes — NaN embedding in DB flows into HNSW unchecked]
- **Difficulty:** easy
- **Location:** src/store/chunks/embeddings.rs:47-50 (`Embedding::new(embedding)` with no is_finite check)
- **Description:** `bytes_to_embedding` (src/store/helpers/embeddings.rs:48) validates byte length but not finiteness. `get_embeddings_by_hashes` then wraps the Vec<f32> in `Embedding::new` — the unchecked constructor — and hands it to HNSW. `cache.rs:test_nan_embedding_roundtrip` already proves NaN round-trips through the cache, so this is reachable via cache→store write. The pipeline's `embedding.rs:53` guards cache hits with `try_new` but the store path has no equivalent. Any NaN in the DB corrupts HNSW traversal (`is_finite` check at hnsw/search.rs:98 catches results but not queries).
- **Suggested fix:** Add `test_get_embeddings_by_hashes_skips_nan_blobs` — write a chunk whose embedding blob bytes decode to `[0.5, f32::NAN, ...]`, call `get_embeddings_by_hashes`, assert the NaN-containing entry is dropped or returned via `try_new` so callers can detect it. Equivalent test for `get_chunk_ids_and_embeddings_by_hashes` at line 103 (same bug). Also a counterpart: `test_bytes_to_embedding_rejects_non_finite`.

## [Missing test: Store::upsert_sparse_vectors — two concurrent writers race the generation counter]
- **Difficulty:** medium
- **Location:** src/store/sparse.rs:130-146 (SELECT splade_generation → INSERT ON CONFLICT in separate statements inside a write tx)
- **Description:** tests/stress_test.rs has `test_concurrent_searches` but nothing exercises concurrent *writers*. `upsert_sparse_vectors` reads splade_generation then writes gen+1, relying on `WRITE_LOCK` (DS-5) to serialize. If DS-5 ever regresses (e.g., a future refactor moves begin_write outside the lock), two writers could both read generation=N and both write N+1, leaving persisted SPLADE indexes with a stale generation that looks valid. No test detects this.
- **Suggested fix:** Add `test_concurrent_upsert_bumps_generation_monotonically` under `#[ignore]` in stress_test.rs — spawn 8 threads, each calling `upsert_sparse_vectors` with distinct chunk_ids, assert `splade_generation()` equals initial + 8 at the end. Regressions in WRITE_LOCK surface as `splade_generation < initial + 8`.

## [Missing test: Store::prune_orphan_sparse_vectors — generation bump-skip branch when zero rows deleted]
- **Difficulty:** easy
- **Location:** src/store/sparse.rs:230-262 (the `if result.rows_affected() > 0` branch)
- **Description:** The audit brief flags this: "prune when rows are 0 (the branch I added that skips the generation bump)". No test. A regression that flips the condition to `>=` or removes it would unconditionally bump the generation on a no-op prune, invalidating the on-disk SPLADE index on every `cqs index` — silent perf regression (the 45s rebuild returns).
- **Suggested fix:** `test_prune_orphan_no_rows_does_not_bump_generation` — insert sparse vectors with all chunk_ids that exist in `chunks` table, read `splade_generation()`, call `prune_orphan_sparse_vectors`, assert result=0 and generation unchanged. Companion: `test_prune_orphan_with_orphans_bumps_generation`.

## [Missing test: Store::prune_orphan_sparse_vectors + splade_generation — zero tests total for either function]
- **Difficulty:** easy
- **Location:** src/store/sparse.rs:230, 270 (both public functions)
- **Description:** Both functions have zero direct tests. `splade_generation` is read on every SPLADE index load (persistence hot path). `prune_orphan_sparse_vectors` is the only user-facing mechanism that keeps sparse_vectors clean. `prune_orphan_sparse_vectors` was dead-code in an earlier audit (EH-16) — no coverage means future refactors can re-orphan it.
- **Suggested fix:** (1) `test_prune_orphan_deletes_rows_for_missing_chunks`: insert chunks A+B, insert sparse for A+B+C, call prune, assert C's sparse rows gone. (2) `test_splade_generation_starts_at_zero_and_is_monotonic`: fresh store returns 0, upsert bumps to 1, another upsert to 2.

## [Missing test: SpladeIndex::save — oversized chunk id path never exercised]
- **Difficulty:** easy
- **Location:** src/splade/index.rs:227-232 (chunk id length try_into u32 error)
- **Description:** The `id.len() > u32::MAX` branch exists but is untested. Rust strings >4GB are unrealistic via normal construction but a defensive check deserves a test, especially given the audit brief flags "upsert with extremely long chunk_ids that exceed u32 byte length" as a sparse.rs gap. In sparse.rs `chunk_id` is a TEXT column with no length cap; a malicious/broken indexer could write a chunk_id at any size and the SPLADE save path would error at serialize time.
- **Suggested fix:** `test_sparse_upsert_long_chunk_id` — upsert a chunk_id of realistic-but-large size (e.g., 100KB string), load_all, verify it round-trips. Companion test in splade/index.rs would require a >4GB String which is impractical — document the invariant as a SPLADE precondition and add `assert!(id.len() <= u32::MAX as usize)` at the sparse.rs call site with a tracing warn.

---

# Robustness — v1.22.0 audit

Two findings in PR #895 (SpladeIndex on-disk persistence). Core `try_into().unwrap()` patterns
in `load()` are proven-safe — the `need()` guard checks body bytes, `header[N..M]` uses fixed
compile-time slice lengths into `[u8; N]`, and `posting_count.saturating_mul(8)` on 64-bit never
saturates (u32::MAX * 8 < usize::MAX). RB-15 through RB-18 from v1.20.0 are confirmed fixed in
source. No new non-test `unwrap()`/`expect()`/`panic!` findings outside the SPLADE persistence path.

## Splade index header fields are NOT covered by the body checksum, enabling OOM panic from minor corruption

- **Difficulty:** easy
- **Location:** src/splade/index.rs:389-391, 405-411, 439-446
- **Description:** `SpladeIndex::load` reads `chunk_count` (bytes 16–24) and `token_count`
  (bytes 24–32) out of the file header, then verifies the **body** checksum (stored at bytes
  32–64) against `blake3(body)`. The header itself is not included in the hash input. A single
  bit flip in the 16-byte range `[16..32]` passes every validation step up through
  `ChecksumMismatch` and then reaches:
  ```
  Vec::<String>::with_capacity(chunk_count_usize)   // line 411
  HashMap::<u32, ...>::with_capacity(token_count_usize)  // line 445-446
  ```
  On 64-bit, `u64::try_into::<usize>()` is the identity, so a corrupted `chunk_count` of
  e.g. `0xFFFFFFFFFFFFFFFF` is accepted and `Vec::with_capacity(usize::MAX)` panics on
  `alloc::raw_vec::capacity_overflow`, crashing the process instead of returning a clean
  `SpladeIndexPersistError`. The same issue applies to `token_count` via the HashMap capacity
  call. Concrete trigger: flip bit 62 of byte 22 on disk — chunk_count becomes ~4.6×10^18,
  checksum still passes because the body is untouched, `load()` panics. Bit rot, disk failure,
  or truncated-write-followed-by-header-corruption can all produce this. The HNSW load path at
  `src/hnsw/persist.rs:539-555` defends against the analogous issue with an explicit
  `max_id_map_size` check before reading.
- **Suggested fix:** Either (1) extend the checksum to cover the entire header except the hash
  bytes — e.g. hash `header[0..32] || body` and store that as the body hash, so any header
  corruption other than the hash field itself is caught before `Vec::with_capacity`; or (2) add
  an explicit sanity bound before the capacity calls: since every chunk consumes at least 4
  bytes for its length prefix and every token entry consumes at least 8 bytes, reject any file
  with `chunk_count_usize > body.len() / 4` or `token_count_usize > body.len() / 8`. Option (2)
  is a two-line fix, requires no format bump, and makes the invariant explicit.

## Splade index load() reads the entire body with no upper bound, enabling unbounded allocation

- **Difficulty:** easy
- **Location:** src/splade/index.rs:395-396
- **Description:** After header validation, `load()` calls
  `reader.read_to_end(&mut body)?` on the open file. There is no check on the file's metadata
  size and no cap like the HNSW loader uses. A corrupted, malicious, or accidentally-grown
  `splade.index.bin` (for example because a previous `save()` was interrupted mid-write and
  the partial temp file was mistakenly renamed, or because the directory is a stale clone from
  a much larger project) causes an unbounded `Vec<u8>` allocation inside `read_to_end`. This
  is OOM before any of the header sanity checks above even fire for the body content. HNSW
  mirrors this exact concern at `src/hnsw/persist.rs:557-570` with explicit per-file
  `hnsw_max_graph_bytes()` / `hnsw_max_data_bytes()` limits; the SPLADE loader landed
  without the matching guard.
- **Suggested fix:** Add a size check mirroring HNSW: stat the file, compare against an
  env-configurable cap like `CQS_SPLADE_MAX_INDEX_BYTES` (default e.g. 2 GB since
  SPLADE-Code on a cqs-sized project is ~100 MB — 20× headroom). Return a new
  `SpladeIndexPersistError::FileTooLarge { size, limit }` variant on overflow. Do this before
  `read_to_end`.

---

# Scaling & Hardcoded Limits — v1.22.0 audit

Eleven findings. PR #891 fixed `upsert_sparse_vectors` INSERT but left the sibling DELETE loop on the old 333 constant, and at least 14 other call sites across `store/` still carry pre-3.32 SQLite `999` assumptions in batch constants and comments. Plus: PR #893's `busy_timeout(5)`/`idle_timeout(30)` on the connection options have no env override, `POOL_MAX_CONNECTIONS = 4` is hardcoded on write opens, `mmap_size = 256MB` is hardcoded in `open()`, the embedder `embed_documents` has its own private `MAX_BATCH = 64` that ignores `CQS_EMBED_BATCH_SIZE`, `MAX_QUERY_BYTES = 32KB` has no escape hatch, and SPLADE `4000`-char truncation is duplicated twice with no env var.

## SHL-31: `upsert_sparse_vectors` DELETE loop still uses `chunks(333)` after PR #891 fixed the INSERT

- **Difficulty:** easy
- **Location:** src/store/sparse.rs:51-62
- **Description:** PR #891 changed the INSERT loop at line 89 to derive from `SQLITE_MAX_VARIABLES = 32766`, but the DELETE loop immediately above (line 53) still uses `for batch in chunk_ids.chunks(333)` with the comment "PF-11: N→ceil(N/333) SQL statements". On a 12k-chunk reindex this produces 37 DELETE statements when 1 would suffice (12k × 1 bind = 12000, still under 32766). Same pre-3.32 mistake the INSERT fix diagnosed. With the secondary token_id index already dropped, the per-statement sqlx overhead here is the only remaining cost in the DELETE phase.
- **Suggested fix:** Replace the literal `chunks(333)` with a derived value from the same SQLite constraint (`(SQLITE_MAX_VARIABLES - SAFETY_MARGIN_VARS)` since it's 1 bind per row). Promote the two constants to `pub(super)` or a shared helper so the DELETE loop pulls from the same source of truth.

## SHL-32: `CHUNK_INSERT_BATCH = 49` is the biggest pre-3.32 waste in the hot indexing path

- **Difficulty:** easy
- **Location:** src/store/chunks/async_helpers.rs:220-242
- **Description:** The primary indexing path batches chunk inserts at `const CHUNK_INSERT_BATCH: usize = 49` because 49 × 20 bind params = 980, "under SQLite's 999 limit". On modern SQLite (32766), the max batch is ~1638 rows. A 12k-chunk reindex currently issues ~245 INSERT statements; the constraint permits ~8. Each statement walks the WAL, updates 4 secondary indexes, and bears sqlx's async dispatch overhead. Same symptom class as #891 on SPLADE-Code 0.6B — at scale this stacks into minutes.
- **Suggested fix:** Derive the constant the same way PR #891 did for sparse vectors:
  ```rust
  const SQLITE_MAX_VARIABLES: usize = 32766;
  const VARS_PER_ROW: usize = 20; // 20 columns pushed per row
  const SAFETY_MARGIN_VARS: usize = 300;
  const CHUNK_INSERT_BATCH: usize =
      (SQLITE_MAX_VARIABLES - SAFETY_MARGIN_VARS) / VARS_PER_ROW;
  ```
  Put these in one module (e.g. `store/helpers/sql_limits.rs`) and import them from every batched-insert site so the next schema change only edits `VARS_PER_ROW` at one place.

## SHL-33: 14 other store call sites hardcode pre-3.32 SQLite batch sizes

- **Difficulty:** easy (mechanical)
- **Location:** Multiple — src/store/types.rs:116,161 (INSERT_BATCH=249, "4 binds × 249 = 996"); src/store/calls/crud.rs:30,32,81,214 (INSERT_BATCH=300 + comment "900 < 999 limit"; line 214 uses 190 for 950/999); src/store/chunks/crud.rs:303 (BATCH_SIZE=132 "132 × 5 = 660 < 999"), :267 (chunks(499)), :484,552 (chunks(500)); src/store/chunks/staleness.rs:86,195 (BATCH_SIZE=100 "~999 param limit"), :402 (BATCH_SIZE=900); src/store/calls/crud.rs:112 (chunks(200) "well under SQLite's 999 limit"); src/store/chunks/async_helpers.rs:345 (chunks(180) "under SQLite 999 limit"); src/store/helpers/sql.rs:5 (`PLACEHOLDER_CACHE_MAX = 999`).
- **Description:** Every one of these carries an explicit "999" reference in the code or comment. None of them are wrong — they're all under the modern limit too — but collectively they represent 10-30x more SQL round trips than modern SQLite permits. PR #891 treated sparse_vectors as a one-off; the same derivation applies everywhere. The `PLACEHOLDER_CACHE_MAX = 999` constant in `sql.rs` also caps which batch sizes get the cached-string fast path. If a caller bumps to e.g. 5000 rows, every call re-builds the placeholder string (minor but unnecessary).
- **Suggested fix:** Introduce `store::helpers::sql_limits` with a single `max_rows(vars_per_row: usize) -> usize` helper (returning `(32766 - 300) / vars_per_row`). Migrate each site in a single follow-up PR. Bump `PLACEHOLDER_CACHE_MAX` to match, or make the cache bucket-sparse (only cache sizes 10, 100, 500, 1000, 5000, 10000).

## SHL-34: `busy_timeout(5s)` and `idle_timeout(30s)` hardcoded on all store opens

- **Difficulty:** easy
- **Location:** src/store/mod.rs:350,372
- **Description:** Both timeouts are literal `Duration::from_secs(5)` / `Duration::from_secs(30)` with no env variable. On WSL `/mnt/c/` or NFS, a large `PRAGMA wal_checkpoint(TRUNCATE)` on close of a 1GB+ DB can legitimately exceed 5 s, producing spurious `SQLITE_BUSY` under concurrent watch+search. The idle timeout (PB-2 "shorter timeout to release WAL locks") was tuned for the small-DB case; at 1+ GB WAL footprint, tearing down connections at 30 s idle forces re-open cost (including the new `quick_check` from #893) on the next query. Neither has an escape hatch.
- **Suggested fix:** Add `CQS_SQLITE_BUSY_TIMEOUT_MS` (default 5000) and `CQS_SQLITE_IDLE_TIMEOUT_SECS` (default 30), read via `OnceLock` following the `embed_batch_size()` pattern. Memory rule: agents are the consumer, knobs are cheap. At very least for the busy timeout — a user running watch + index concurrently on WSL should have a way to bump it to 30 s without recompiling.

## SHL-35: Connection pool `max_connections = 4` hardcoded on write opens, no env override

- **Difficulty:** easy
- **Location:** src/store/mod.rs:281 (open() uses 4), also pinned into StoreOpenConfig at :256
- **Description:** `Store::open()` — the write/default path — hardcodes a 4-connection pool. That choice was benchmarked back when indexing was ~1k chunks; the tokio worker thread count is tied to it at line 339 (`worker_threads(config.max_connections as usize)`). For bulk reindex on an 8+ core machine, 4 parallel SQLite writer connections + 4 worker threads is a defensible default but undersized for e.g. the 64-core workstation; for a watch-only server that barely writes, 4 is wasteful. Nothing else in the project is this aggressive about hardcoding a connection pool — the cache path has a dedicated `max_connections(1)` comment justifying its smaller size.
- **Suggested fix:** Add `CQS_POOL_MAX_CONNECTIONS` env var (default 4), read once via `OnceLock` at `open()` time and threaded into `StoreOpenConfig::max_connections`. The tokio worker thread count will follow automatically. Document that raising it is only useful on indexing workloads and that the SQLite single-writer constraint still serializes actual writes (via the `WRITE_LOCK` mutex at `mod.rs:51`).

## SHL-36: `mmap_size = 268435456` (256 MB) hardcoded in `Store::open` and `open_readonly_pooled`, no env override

- **Difficulty:** easy
- **Location:** src/store/mod.rs:282, :304, :320
- **Description:** Three hardcoded mmap_size pragma strings: "268435456" (256MB) for `open()` and `open_readonly_pooled()`, "67108864" (64MB) for `open_readonly()`. For a 1.1 GB cqs index, 256 MB mmap means only ~25% of the DB can be in the OS page cache at once; cold SPLADE lookups can miss mmap and fall back to read syscalls. For an operator running cqs on a small VPS, 256 MB × 4 connection = 1 GB of virtual mapping is undersized or outsized depending on the machine. Unlike `CQS_HNSW_MAX_*` which are env-readable (SHL-17 fix), the mmap size is not.
- **Suggested fix:** Add `CQS_SQLITE_MMAP_BYTES` (default 268435456). For the `open_readonly()` (reference stores) path keep the 64 MB default but accept the same override. Thread it into `StoreOpenConfig::mmap_size` (currently `&'static str`; change to `String`).

## SHL-37: `Embedder::embed_documents` has a private `MAX_BATCH = 64` that ignores `CQS_EMBED_BATCH_SIZE`

- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:507-521
- **Description:** The pipeline path uses `cli::pipeline::types::embed_batch_size()` which reads `CQS_EMBED_BATCH_SIZE`. `Embedder::embed_documents` is called from outside the pipeline (e.g. `gather.rs`, HyDE, doc-comment rewrite) and uses its own `const MAX_BATCH: usize = 64`, *ignoring* the env var. A user who set `CQS_EMBED_BATCH_SIZE=16` to avoid GPU OOM during indexing still gets 64-size batches during ad-hoc embedding — same SHL-27 anti-pattern that was fixed for `ENRICH_EMBED_BATCH` but missed here.
- **Suggested fix:** Replace `const MAX_BATCH: usize = 64` with `cli::pipeline::types::embed_batch_size()`. That constant lives in the CLI crate — if the embedder can't import it, promote `embed_batch_size()` to a library-level helper in `lib.rs` or `embedder/mod.rs` and call it from both places. Same fix pattern as PR #891.

## SHL-38: SPLADE 4000-char truncation duplicated in `encode` and `encode_batch`, hardcoded, no env override

- **Difficulty:** easy
- **Location:** src/splade/mod.rs:368-382 (`encode`), :533-548 (`encode_batch`)
- **Description:** Both functions truncate `text.len() > 4000` to 4000 chars before tokenization. The constant is literal, duplicated, and has no rationale tying it to the model's max_seq_length. For SPLADE-Code 0.6B (which accepts 512 tokens ≈ ~2000 code chars), 4000 is over-budget — tokenization work is wasted. For a hypothetical longer-context SPLADE variant, 4000 silently truncates useful signal. This was reported under RB-13 as "add an input cap" but the *value* was never justified or made configurable.
- **Suggested fix:** Extract to `splade::MAX_INPUT_CHARS` (or even better, derive from `self.tokenizer.max_seq_length * AVG_CHARS_PER_TOKEN` where `AVG_CHARS_PER_TOKEN` is a small constant like 4). Add `CQS_SPLADE_MAX_INPUT_CHARS` env override defaulting to 4000. De-duplicate the two call sites into a shared helper function.

## SHL-39: `Embedder::MAX_QUERY_BYTES = 32 * 1024` hardcoded, no env override

- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:534
- **Description:** Queries longer than 32 KB are truncated at `embed_query` with `tracing::warn!`. Default is a reasonable guard but hardcoded with no escape hatch, and emits only a tracing warning (invisible at default log level). A user programmatically feeding a large HyDE-generated document as a query will hit this silently. Similar to SHL-19 for LLM content chars, already fixed — but only for the LLM path, not for embeddings.
- **Suggested fix:** Add `CQS_EMBED_MAX_QUERY_BYTES` env var (default 32768). Also emit the truncation via `eprintln!` at non-verbose log levels, not just `tracing::warn!`, so a caller piping large text sees the warning without `RUST_LOG=warn`.

## SHL-40: HNSW streaming build batch size `10_000` hardcoded in two places, no env override

- **Difficulty:** easy
- **Location:** src/cli/commands/index/build.rs:680 (`build_hnsw_index_owned`), :716 (`build_hnsw_base_index`)
- **Description:** Both HNSW builders call `store.embedding_batches(HNSW_BATCH_SIZE)` with `const HNSW_BATCH_SIZE: usize = 10_000`. At 1024-dim BGE-large × 4 bytes, 10k embeddings = ~40 MB per batch before the HNSW build allocates its own working memory. Fine for most machines, but: (a) two identical constants in two functions — any tune of one silently drifts from the other, and (b) no env override for the memory-constrained case (small VPS) or the plenty-of-RAM case (workstation where 100k would reduce SQLite pagination overhead). This is also the value that determines how many `embedding_batches` cursor pages are produced — the inner loop of the streaming HNSW build.
- **Suggested fix:** Extract into `hnsw::DEFAULT_BUILD_BATCH_SIZE` or a `hnsw_build_batch_size()` helper reading `CQS_HNSW_BUILD_BATCH_SIZE`. Use the same helper in both builders so they stay in sync.

## SHL-41: `SAFETY_MARGIN_VARS = 300` in sparse insert loop is reasonable but not scaled with row tuple width

- **Difficulty:** easy
- **Location:** src/store/sparse.rs:86-88
- **Description:** PR #895's comment says "headroom for one extra column on a max-size batch". The math: one extra column at max batch = 1 × 10822 = 10822 > 300. The 300 margin is *not* sized to absorb adding a column; it's a generic headroom. That's fine in practice (modern SQLite's 32766 limit has slack), but the rationale as written is wrong. If a reviewer sees the comment and actually adds a column, the batch will overflow. Either the margin needs to grow to `ROWS_PER_INSERT * (NEW_COLS)` or the comment should say "generic safety margin, NOT a full extra column". For scaling generally, this is a case where the right answer is probably to re-derive the three constants at every call site — or better, centralize them, as SHL-33 suggests.
- **Suggested fix:** Change the comment to reflect reality ("Generic headroom; adding a new bind column requires increasing VARS_PER_ROW, not this margin"). Centralize the three constants as part of SHL-33's `sql_limits` module so the next schema change flows through automatically.

---

# Algorithm Correctness — v1.22.0 audit

4 findings: one hard correctness bug in SPLADE fusion (fused scores discarded), two router substring false-positives, and one bootstrap_ci boundary panic.

## SPLADE hybrid fusion scores are discarded; alpha is only a prefilter knob
- **Difficulty:** hard
- **Location:** src/search/query.rs:502-540 (fusion build), src/search/query.rs:700-734 (re-score)
- **Description:** `search_hybrid` computes `score = alpha * dense + (1 - alpha) * sparse` at line 518, sorts into `fused`, truncates to `candidate_count = max(limit*5, 100)`, then passes only the **IDs** to `search_by_candidate_ids_with_notes`. That function re-fetches candidates from SQL (losing the fused ranking via `IN (...)` reorder), calls `score_candidate` (cosine + name boost only — `src/search/scoring/candidate.rs:231`), and sorts by the re-computed cosine score at line 731. The SPLADE `alpha` knob therefore only acts as a candidate *selector* when `candidate_count < dense+sparse union size`; for all non-boundary cases the final ranking is pure cosine, regardless of alpha. Concrete input: query with two candidates A, B where cosine(A)=0.30, cosine(B)=0.40, sparse(A)=0.80, sparse(B)=0.10. At `alpha=0.3` the fusion formula says A (0.79) > B (0.21), but since both land inside `candidate_count`, the re-score ranks B > A. The special-case `alpha <= 0.0` branch at line 507 has the same fate — `1.0 + s` is computed, then thrown away. This makes the SPLADE eval sweep a no-op on the final returned ordering and silently wastes the SPLADE inference cost.
- **Suggested fix:** thread the fused score through. Either (a) add `pub fn search_by_candidate_ids_with_scores` that preserves a `HashMap<String, f32>` of pre-computed scores and skips the cosine recompute when present, or (b) change `search_hybrid` to do its own finalize pass (fetch → parent dedup → type boost → truncate) without delegating to the cosine-only candidate path. Option (a) is smaller: pass `fused: Vec<IndexResult>` instead of `candidate_ids: Vec<&str>` and let the re-score apply note/demotion multipliers to `r.score` instead of recomputing from the embedding.

## Router negation classifier matches `"not "` inside `cannot`, `"no "` inside `piano`/`nano`/`volcano`/`casino`
- **Difficulty:** easy
- **Location:** src/search/router.rs:193-204 (`NEGATION_WORDS`), src/search/router.rs:288 (classification)
- **Description:** The check is `NEGATION_WORDS.iter().any(|w| query_lower.contains(w))` — a raw substring match with no word-boundary enforcement. `"not "` has a trailing space but no leading space, so `"cannot find module"` matches at bytes 3-6 (`not ` inside `cannot `). Similarly `"no "` matches inside `"piano samples"` (bytes 3-5 `no `), `"nano service"` (bytes 2-4), `"volcano eruption"` (bytes 5-7), `"casino royale"` (bytes 4-6), `"dynamo library"`... also `"avoid "` wouldn't false-fire but `"don't "`/`"doesn't "` have their own false-positive surface inside words like `"donut "`, `"unodontic "`, etc. Concrete input: `classify_query("cannot find module")` returns `Classification { category: Negation, confidence: High, strategy: DenseBase }`, routing a compile-error lookup to the non-enriched index and stripping LLM summaries from the search signal. For a real session, every query mentioning `piano`, `nano`, `volcano`, `casino`, `cannot` is silently misrouted to DenseBase.
- **Suggested fix:** require word boundaries. Cheapest: tokenize `words: Vec<&str>` (already computed at line 274) and check `words.iter().any(|w| NEGATION_TOKENS.contains(w))` with `NEGATION_TOKENS = ["not", "without", "except", "never", "avoid", "no", "don't", "doesn't", "shouldn't", "exclude"]` (no trailing spaces). The existing `words` tokenization already exists — this swap is one line + removing the trailing-space suffixes from the constant.

## `bootstrap_ci(&values, 0)` panics with underflow on empty `estimates`
- **Difficulty:** easy
- **Location:** tests/eval_common.rs:187-194
- **Description:** When `n_resamples == 0`, the resample loop at 173 doesn't run, so `estimates: Vec<f64>` is empty. Line 187: `lo_idx = ((0 * 0.025).ceil() as usize).saturating_sub(1)` = 0 (OK). Line 188: `hi_idx = (0 * 0.975).ceil() as usize - 1` = `0usize - 1` — **integer underflow**, panics in debug, wraps to `usize::MAX` in release. Line 192: `estimates[lo_idx.min(estimates.len() - 1)]` = `estimates[0.min(0 - 1)]` — subtraction underflow again on `estimates.len() - 1` where `len() == 0`. Concrete input: `bootstrap_ci(&[0.5, 0.3], 0)` panics with integer underflow or out-of-bounds index depending on build mode. Only reachable if a caller passes `n_resamples = 0`; currently all call sites pass positive values, but the harness is test infra and future changes could hit this.
- **Suggested fix:** early return for `n_resamples == 0`:
  ```rust
  if n_resamples == 0 {
      return MetricWithCI { value: point, ci_lower: point, ci_upper: point };
  }
  ```
  Place it after the `point` calculation at line 159.

## `is_test_chunk` demotes any name starting with `"Test"`, hitting production types
- **Difficulty:** easy
- **Location:** src/lib.rs:241
- **Description:** `name.starts_with("Test")` (capital T, no underscore) matches production types like `TestRegistry`, `TestRunner`, `TestHarness`, `TestContext`, `TestConfig` — common identifiers in test-framework code, driver code, and DI container registration. These are production chunks, not test cases. `score_candidate` then applies `importance_test = 0.70` demotion at `candidate.rs:28-30`, knocking their rank 30% in every search result. The existing tests at `lib.rs:755-779` deliberately exclude `"testing_utils.rs"` and `"attest.rs"` but not `"TestRegistry"`/`"TestBuilder"`/`"TestHarness"` names. Concrete input: a project containing `pub struct TestHarness { ... }` in `src/harness/mod.rs` (not a test file). `is_test_chunk("TestHarness", "src/harness/mod.rs")` returns true at line 247 because `"TestHarness".starts_with("Test")` is true, and the chunk's search rank is multiplied by 0.70. Users searching for "test harness setup" get the type demoted below irrelevant results.
- **Suggested fix:** require a word boundary after the `Test` prefix. Replace `name.starts_with("Test")` with `name.starts_with("Test") && name.chars().nth(4).is_some_and(|c| c == '_' || c.is_uppercase() == false || c == ':')` — or more cleanly, match `Test_` and `TestCase` patterns but not generic `Test*`. A pragmatic fix: check for `"Test"` followed by a lowercase letter (`Test_case`, `TestRunner` → keep demoting since it looks like xUnit runner) vs uppercase+lowercase (`TestClass` — still ambiguous). Honestly, this prefix should probably require `Test` followed by nothing or an underscore (`Test`, `Test_foo`), matching the `test_` lower-case path more consistently. Alternatively drop the `starts_with("Test")` check and rely on file-path signals; test frameworks in Rust/Go/Python put tests in designated files that already catch at lines 253-268.

---

# Extensibility — v1.22.0 audit

7 findings. Language registry macro is well-factored, but 4 seams still bypass it: `classify_query` hardcodes 21 of 54 language names and 17 chunk patterns, `is_test_chunk` duplicates `test_path_patterns` logic, `parse_source` hardcodes per-language custom parser dispatch, and `cmd_query_project` uses a non-scaling boolean switch for SearchStrategy that leaves `DenseWithSplade` as an orphaned variant. Two smaller findings: no `INDEX_DB_FILENAME` constant despite 40+ literals, and `ModelConfig::from_preset` has no enumerated preset list for error messages / validation.

## EXT-7: `classify_query` LANGUAGE_NAMES hardcodes 21 of 54 supported languages
- **Difficulty:** easy
- **Location:** src/search/router.rs:221-243 (`LANGUAGE_NAMES`), consumed by `is_cross_language_query` at :458-471
- **Description:** The cross-language detection list is a static `&[&str]` listing only 21 languages. The registry has 54 registered via the macro (including `cuda`, `glsl`, `julia`, `gleam`, `zig`, `nix`, `r`, `dart`, `fsharp`, `ocaml`, `powershell`, `bash`, `solidity`, `vbnet`, `gleam`, etc.), none of which are detected by the classifier. Adding the 55th language (or just querying "Rust equivalent of Dart's factory constructor") silently falls through to the default strategy. Every new language addition must also remember to edit this list — a second registration point the macro was supposed to eliminate.
- **Suggested fix:** Delete `LANGUAGE_NAMES`, use `crate::language::REGISTRY.all().map(|d| d.name)` inside `is_cross_language_query` (compute lazily via `LazyLock<HashSet<&'static str>>` to match existing `COMMON_TYPES` pattern in `focused_read.rs:17`).

## EXT-8: `extract_type_hints` hardcodes 17 patterns, misses 13+ ChunkType variants
- **Difficulty:** easy
- **Location:** src/search/router.rs:515-549 (`extract_type_hints`)
- **Description:** The pattern table maps only 10 of 29 ChunkType variants to NL phrases. Adding a new ChunkType (e.g., the future `Record`, or existing-but-missing `Function`, `Method`, `Macro`, `Namespace`, `Constructor`, `TypeAlias`, `Event`, `Property`, `Service`, `StoredProc`, `Extern`, `Modifier`, `Middleware`, `Delegate`, `Object`, `Impl`, `Variable`) requires adding new `("all Xs", ChunkType::X)` rows here. The test `test_all_chunk_types_classified` at src/language/mod.rs:963 guards `is_code()`/`is_callable()` but not this list. New chunk types silently miss type-hint boost for natural-language queries.
- **Suggested fix:** Derive patterns from `ChunkType::ALL` using `ct.human_name()` as the base (already exists at src/language/mod.rs:577) — e.g., `"all {plural}"` and `"every {singular}"` for each variant. Requires a `plural_name()` helper (or an s-suffix rule) on ChunkType. Adding a new variant then auto-extends the classifier.

## EXT-9: `is_test_chunk` parallel source of truth for test file detection
- **Difficulty:** medium
- **Location:** src/lib.rs:238-269 (`is_test_chunk`), shadowed by `FALLBACK_TEST_PATH_PATTERNS` at src/store/calls/mod.rs:134 and `build_test_path_patterns` at :156
- **Description:** `is_test_chunk` duplicates test-path detection that should be sourced from `LanguageDef::test_path_patterns` (via `REGISTRY.all_test_path_patterns()`). It hardcodes only Go and Python suffixes (`_test.go`, `_test.py`), plus the shared `/tests/`, `_test.`, `.test.`, `_spec.`, `.spec.` patterns. Every other registered language's test patterns (54 of 54 already defined: Rust, Java, Erlang, C, C++, Dart, Elixir, Gleam, Haskell, etc.) are invisible to `is_test_chunk`. Since this is called from `chunk_importance` (src/search/scoring/candidate.rs:28) and `filter_candidates` in dead code (src/store/calls/dead_code.rs:139), test demotion and dead-code elision miss 50+ languages' test files. The existing `all_test_path_patterns()` already solves this problem for SQL queries elsewhere.
- **Suggested fix:** Rewrite `is_test_chunk` to check name patterns (language-agnostic prefix rules stay) and then iterate `REGISTRY.all_test_path_patterns()` matching against the file path. Language patterns are in SQL `LIKE` form (e.g., `%_test.go`) — convert once at startup into a regex or simple suffix/substring set via `LazyLock`.

## EXT-10: `parse_source` hardcodes grammar-less language dispatch
- **Difficulty:** easy
- **Location:** src/parser/mod.rs:232-244 (`parse_source` match), src/parser/mod.rs:205-211 (`parse_file` L5X extension sniffing)
- **Description:** `parse_source` special-cases `Language::Aspx` and defaults everything else with `grammar: None` to markdown. Adding a third non-tree-sitter language (say, the already-hardcoded L5X / L5K formats at `parse_file:206-210` or a future non-tree-sitter format) requires editing this match arm plus the extension prefilter in `parse_file`. Two separate dispatch layers for "no tree-sitter" — one in `parse_file` keyed on extension strings, one in `parse_source` keyed on Language variants. The LanguageDef struct at src/language/mod.rs:237 has 29 fields but no `custom_parser: Option<fn(&str, &Path, &Parser) -> Result<Vec<Chunk>, ParserError>>` field to let a grammar-less language self-register its parser through the macro.
- **Suggested fix:** Add `custom_parser: Option<CustomParserFn>` to `LanguageDef`. Move `parse_aspx_chunks`, `parse_markdown_chunks`, and `parse_l5x_chunks` into their respective language definitions. Replace the match in `parse_source` with `def.custom_parser.map(|f| f(source, path, self))`. Migrate L5X to a real Language variant so the `parse_file` prefilter becomes unnecessary. Result: adding a non-tree-sitter language is one line in the macro, one `fn` implementation, zero edits to `parse_source`/`parse_file`.

## EXT-11: `SearchStrategy` dispatch uses boolean switches, `DenseWithSplade` orphaned
- **Difficulty:** medium
- **Location:** src/search/router.rs:71-85 (`SearchStrategy` enum), src/cli/commands/search/query.rs:279-303 (`cmd_query_project` branching on `Some(DenseBase)`)
- **Description:** `SearchStrategy::DenseWithSplade` is a defined variant with zero production callers (found only at src/search/router.rs:79 declaration and :93 Display impl). `classify_query` never returns it; nothing consumes it. The pending roadmap item "Selective SPLADE routing" (ROADMAP.md:33) requires: (1) `classify_query` returning `DenseWithSplade` for `CrossLanguage`, (2) `cmd_query_project` branching on that variant. The current dispatch at query.rs:279 is `use_base = matches!(ctx.routed_strategy, Some(DenseBase))` — a boolean, not a strategy dispatch. Adding `DenseWithSplade` means extending it to an N-way match, and every future strategy adds another branch. This doesn't scale and the orphaned variant proves it: the enum grew but the dispatch site didn't.
- **Suggested fix:** Convert `cmd_query_project` to a match on `routed_strategy`, with explicit arms for `DenseDefault`, `DenseBase`, `DenseWithTypeHints`, `DenseWithSplade`, and `NameOnly` (or route `NameOnly` earlier). Either that, or pull the per-strategy logic into a `SearchStrategy::execute()` method. Then land the pending PR to route cross-language queries to `DenseWithSplade`, at which point the enum variant finally has a caller.

## EXT-12: No `INDEX_DB_FILENAME` constant; `"index.db"` literal in 40+ sites
- **Difficulty:** easy
- **Location:** 40+ sites — representative: src/cli/store.rs:19, src/cli/batch/mod.rs:141/194/581/614/794/850, src/cli/commands/resolve.rs:60, src/cli/commands/infra/reference.rs:125/200/234/325, src/cli/commands/infra/doctor.rs:86/232, src/cli/commands/io/notes.rs:167, src/cli/commands/io/diff.rs:99, src/cli/commands/io/drift.rs:94, src/cli/commands/index/build.rs:67, src/cli/watch.rs:247, src/project.rs:494, src/reference.rs:92, src/store/calls/cross_project.rs:93, src/impact/cross_project.rs (tests), src/store/metadata.rs (tests).
- **Description:** The SPLADE index has `SpladeIndex::SPLADE_INDEX_FILENAME` (src/splade/index.rs:35) but the primary DB file is a bare string literal. Any future rename (e.g., versioned names like `index.v17.db`, or a separate file for a second sparse-index type) means grepping ~40 sites. Both `".cqs/index.db"` (project.rs:494, cross_project.rs:79) and `cqs_dir.join("index.db")` forms coexist.
- **Suggested fix:** Add `pub const INDEX_DB_FILENAME: &str = "index.db";` in `src/lib.rs` or `src/store/mod.rs` and route all productions sites through it. Tests can continue using the literal; but production call paths should use the constant.

## EXT-13: `ModelConfig::from_preset` has no enumerated preset list for validation/errors
- **Difficulty:** easy
- **Location:** src/embedder/models.rs:94-101 (`from_preset`), consumed by :106-138 (`resolve`)
- **Description:** Adding a 4th preset requires editing 3 places: a new `fn foo_model()`, a new match arm in `from_preset`, and the tracing warning "Unknown model from CLI flag" at :116 which doesn't list valid options. There is no `ModelConfig::all_preset_names()` helper, so the CLI can't produce an error message like "unknown model 'bge-larg'; valid: bge-large, e5-base, v9-200k". `Language::valid_names_display()` exists as the pattern (src/language/mod.rs:157). The language macro makes adding a new language one line; adding a model preset requires parallel surgery in at least three locations with no compile-time guarantee that all three stayed in sync.
- **Suggested fix:** Add `ModelConfig::all_presets() -> &'static [(&'static str, fn() -> ModelConfig)]` or similar registry. Use it in `from_preset` (replace the match), in tracing warnings (list valid options), and in `CqsError::UnknownModel` error messages. Alternatively, use a macro like `define_presets!` mirroring `define_languages!` to enforce one-line additions.

---

**Known architectural boundary (not a bug):** `CrossProjectContext` at src/store/calls/cross_project.rs:59 is call-graph-only. Adding a federated dense search mode (query embedding executed across multiple project HNSW indexes with weight-merged results) would require: (1) adding `graphs: HashMap<usize, Arc<dyn VectorIndex>>` or similar to the struct, (2) teaching query.rs to prefer `CrossProjectContext::search_federated` over `build_vector_index`, (3) a result merge strategy. This is by design — `cross_project` was scoped to callers/callees/test-map/impact. Documenting as a known boundary since the prompt asked about federated queries.

**Tracked:** EXT-2 (CLI/batch dual registration, src/cli/definitions.rs + src/cli/batch/commands.rs) already deferred per docs/audit-triage-v1.20.0-pre.md as a design issue. Not re-reported.

---

# Platform Behavior — v1.22.0 audit

10 findings. Centred on SPLADE persistence (PR #895) gaps in WSL/Windows/cross-device parity relative to HNSW persistence, plus watch-mode SPLADE staleness.

## PB-NEW-1: SPLADE save has no file locking — concurrent save races can produce torn file

- **Difficulty:** medium
- **Location:** src/splade/index.rs:208-333
- **Description:** `SpladeIndex::save` writes to a temp file + rename but takes no lock of any kind. Compare `HnswIndex::save` at `src/hnsw/persist.rs:216-224`, which opens `{basename}.hnsw.lock` and calls `lock_file.lock()` before touching any file. Two `cqs` processes can race on SPLADE save when `cqs index` runs from one shell while `cqs search` from another triggers `SpladeIndex::load_or_build` (which re-persists when the on-disk file is stale). Both build their own Vec<u8> body, both rename onto the same final path — last writer wins, but depending on filesystem ordering the generation counter can disagree with the body, and the HNSW lock only protects HNSW files. Additionally, `cqs watch` reading `splade.index.bin` while `cqs index` is renaming it can see `ErrorKind::NotFound` (unix: brief gap; windows: `remove_file` + `rename` gap — see PB-NEW-3).
- **Suggested fix:** Add a `splade.lock` file + `lock()` / `lock_shared()` calls at the head of `save()` and `load()`, mirroring `hnsw::persist::save`. Take the lock on the HNSW lock file if you want a single lock for both (they're always saved/loaded in tandem).

## PB-NEW-2: SPLADE save emits no WSL advisory-locking warning — same NTFS/9P caveats as HNSW, silent

- **Difficulty:** easy
- **Location:** src/splade/index.rs:208 (save); src/hnsw/persist.rs:85-94 (reference `warn_wsl_advisory_locking`)
- **Description:** HNSW save/load calls `warn_wsl_advisory_locking(dir)` which emits a one-time warning on WSL/NTFS mounts: `"HNSW file locking is advisory-only on WSL/NTFS — avoid concurrent index operations"`. PR #895's `SpladeIndex::save` is persisted in the same directory (`.cqs/splade.index.bin`), inherits the same 9P advisory-locking constraint, but never emits the warning. A user running two cqs instances on `/mnt/c/Projects/...` gets one HNSW warning and zero SPLADE warnings, even though SPLADE corruption is equally possible (and more likely, per PB-NEW-1, since SPLADE has no lock at all). Related: the warning is process-wide via an `AtomicBool`, so even if you add SPLADE locking and re-use the HNSW warning function, the warning already fires for HNSW first and won't fire again for SPLADE.
- **Suggested fix:** After adding SPLADE locking (PB-NEW-1), call `warn_wsl_advisory_locking(parent)` from `SpladeIndex::save` and `::load`. Rename the warning text to say "HNSW/SPLADE" or hoist it to a crate-level function. If PB-NEW-1 is deferred, at minimum emit a separate one-shot warning when the SPLADE file lives under `/mnt/*` on WSL and two processes appear to be saving it concurrently (PID check is overkill; just warn on WSL).

## PB-NEW-3: SPLADE Windows save is non-atomic — remove_file + rename has a crash window and TOCTOU race

- **Difficulty:** medium
- **Location:** src/splade/index.rs:319-325
- **Description:** The Windows branch reads:
  ```rust
  #[cfg(windows)]
  { if path.exists() { std::fs::remove_file(path)?; } }
  std::fs::rename(&tmp_path, path)?;
  ```
  This has three platform-specific problems: (a) crash between `remove_file` and `rename` leaves zero splade.index.bin on disk — next open rebuilds from SQLite (~45s), worse than the pre-PR #895 state where at least the old file survived; (b) `path.exists()` → `remove_file` → `rename` is a 3-call TOCTOU sequence: a concurrent `cqs` process that creates the file between steps 1 and 2 triggers an unrelated failure; (c) on native Windows, `remove_file` fails with `ERROR_SHARING_VIOLATION` (os error 32) if another process has the file memory-mapped or open for read (e.g., a long-running `cqs query` batch mode holding `SpladeIndex` in RAM), even though the mmap is orthogonal to the rename. The POSIX `rename-over-existing` is atomic for free; Windows has `MoveFileExW(..., MOVEFILE_REPLACE_EXISTING)` and `ReplaceFileW` which are the correct atomic-replace primitives. `std::fs::rename` on Windows uses `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING` internally since Rust 1.46, so the `remove_file` call is not merely redundant — it actively makes the code worse. The comment "the rename may fail if the destination exists" is out of date.
- **Suggested fix:** Delete the `#[cfg(windows)] { remove_file }` block entirely. `std::fs::rename` on Windows has handled the replace-existing case for years (1.46+). The same pattern appears in other cqs files that used to have this workaround — search for `cfg(windows)` + `remove_file` and remove them consistently.

## PB-NEW-4: SPLADE save has no cross-device rename fallback — Docker overlayfs + WSL 9P failures hard-fail

- **Difficulty:** medium
- **Location:** src/splade/index.rs:325
- **Description:** `SpladeIndex::save` does `std::fs::rename(&tmp_path, path)?` with no `ErrorKind::CrossesDevices` handling. Compare `HnswIndex::save` at `src/hnsw/persist.rs:412-445`, which falls back to `fs::copy` → `set_permissions(0o600)` → `fs::rename` when the direct rename fails (Docker overlayfs, NFS, bind mounts, and — relevant here — WSL `/mnt/c` writes to an NTFS junction that points to a different volume). The SPLADE temp file lives in the same parent as the target (`.cqs/.splade.index.bin.{suffix}.tmp`), so in practice they'll normally be on the same device. However: (a) `.cqs` directory can itself be a bind mount on Docker/CI; (b) WSL 9P has been observed returning `ErrorKind::PermissionDenied` on rename across mount-table oddities even within a single `/mnt/c/...` path. The current code bubbles a raw IO error up to `tracing::warn!` in `load_or_build` and continues with an in-memory index only — so every subsequent `cqs search` invocation pays the full rebuild cost indefinitely, silently.
- **Suggested fix:** Copy the cross-device fallback from `hnsw/persist.rs:412-445` into `splade/index.rs:325`: on rename error, `fs::copy` the temp to a second temp in the target directory, `set_permissions(0o600)` on the copy, then rename the copy into place. Log which path was taken so ops can distinguish "save failed entirely" from "save took the slow path."

## PB-NEW-5: SPLADE save does not fsync the parent directory — rename can be lost on power-cut

- **Difficulty:** medium
- **Location:** src/splade/index.rs:314-325
- **Description:** The save sequence is: `writer.flush()` → `writer.get_ref().sync_all()` (file contents durable) → `std::fs::rename(tmp_path, path)`. On POSIX (ext4, xfs, btrfs), the rename operation itself is not persisted to the parent directory until the parent is fsynced. A power-cut between `rename` and the next journal commit leaves the directory in a state where the temp file name may or may not have been replaced by the final name — you can end up with either the new file at `splade.index.bin`, or the old file, or both the temp and old. On WSL `/mnt/c` the 9P-to-NTFS bridge offers weaker durability: NTFS's metadata journaling is not even in the same trust domain as the Linux side. `HnswIndex::save` has the same gap (doesn't fsync the parent), so this is a pre-existing class issue, but #895 introduces a new file subject to it. For a rebuildable search index the durability cost is "45s rebuild on next open" — acceptable, but worth a comment rather than silent breakage.
- **Suggested fix:** After `std::fs::rename(&tmp_path, path)?`, on unix open the parent directory and call `File::sync_all()` on it (`std::fs::File::open(parent)?.sync_all()?`). Windows's NTFS doesn't require directory fsync (metadata is journaled with the rename). If you'd rather not pay the fsync cost per save, at least document explicitly that splade.index.bin is best-effort-durable and a power-cut will trigger a rebuild (which is already the fallback).

## PB-NEW-6: Watch mode does not handle SPLADE at all — sparse vectors go stale silently

- **Difficulty:** hard
- **Location:** src/cli/watch.rs:684-747 (`reindex_files`), src/cli/watch.rs:300-318 (no splade state in `WatchState`)
- **Description:** `cqs watch` never touches SPLADE. It calls `reindex_files` which parses, embeds dense vectors, and upserts chunks, but does not run the SPLADE encoder, does not upsert `sparse_vectors`, and does not rebuild/persist `splade.index.bin`. Consequences: (a) new chunks have no sparse vectors, so hybrid search silently falls back to dense-only for anything added during a watch session; (b) deleted file chunks leave orphan sparse vector rows (`prune_orphan_sparse_vectors` is not called from watch); (c) the `splade_generation` counter is NOT bumped by watch-mode writes, so the on-disk `splade.index.bin` keeps serving stale content on the next `cqs search` WITHOUT triggering a rebuild via the generation mismatch path. This interacts badly with WSL: users commonly run `cqs watch` on WSL expecting it to catch up with file changes, and the WSL inotify-over-9P already loses events. Now there's a second tier of silent data drift on top. The effect is worst on WSL `/mnt/c` because that's where users most commonly rely on watch (autocatchup expected, but in practice manual `cqs index` is recommended per MEMORY.md).
- **Suggested fix:** Option A (match index pipeline): watch's reindex path should mirror `cmd_index` — encode sparse vectors after dense embed, call `store.upsert_sparse_vectors`, then rebuild the in-memory SpladeIndex from the store and call `save()`. Option B (minimal): after any watch-mode write, unconditionally bump `splade_generation` (a plain `UPDATE metadata SET value = value + 1 WHERE key = 'splade_generation'`) so the next `cqs search` sees a generation mismatch and triggers a rebuild-from-SQLite (still slow, but at least correct). Option C: document that watch does not maintain SPLADE and the user must run `cqs index` to refresh sparse vectors. Option B is the right default — it's one SQL statement and preserves correctness without requiring the embedder to load a second ONNX model in watch mode.

## PB-NEW-7: SPLADE save builds 60-100MB body in memory before writing — blocks watch loop on slow filesystems

- **Difficulty:** easy
- **Location:** src/splade/index.rs:218-315
- **Description:** `save()` builds the entire serialized body into a `Vec<u8>` (`Vec::with_capacity(estimate_body_size(...))` at line 220), hashes it in one pass, then writes it. For SPLADE-Code 0.6B at cqs's chunk count the comment says "60-100MB." On WSL `/mnt/c`, writes over 9P to NTFS run at roughly 30-100MB/s depending on load (vs. 1-3GB/s on native ext4), so the `write_all(&body)` + `sync_all()` together can block the calling thread for 1-5+ seconds. This is fine from the CLI path (it's already on the slow path), but the watch loop processes events at 100ms intervals (`rx.recv_timeout(Duration::from_millis(100))`) — a multi-second sync block in the middle of an event burst means `pending_files` can overflow its 10,000-entry cap (`max_pending_files()`) and drop events silently via `tracing::warn!(max = max_pending_files(), ...)`. This interacts with PB-NEW-6: if watch is ever fixed to handle SPLADE, naively calling `save()` from the watch loop will trigger this every burst. (Not a bug today since watch doesn't save SPLADE; flagging because the "proper fix" for PB-NEW-6 will trip over this.)
- **Suggested fix:** When fixing PB-NEW-6 with option A, run the SPLADE save on a background tokio task or std::thread spawned off the watch loop so the sync block doesn't stall event collection. Alternatively, coalesce multiple pending saves with a short debounce (e.g., 5 seconds of "splade dirty" before actually writing) — the save cost doesn't scale with the number of changes, only the total index size, so coalescing saves 100% of redundant writes.

## PB-NEW-8: Quick check runs `PRAGMA quick_check(1)` on write opens — 43s on WSL /mnt/c per PR #893

- **Difficulty:** easy
- **Location:** src/store/mod.rs:430-441
- **Description:** PR #893 downgraded `PRAGMA integrity_check` to `PRAGMA quick_check(1)` on write opens and skipped it entirely on read-only opens. The commit message says 43s was a notable improvement. However, `quick_check` still touches every B-tree root and walks the free list, which on WSL `/mnt/c` against a 1GB+ index.db is dominated by 9P round-trip latency (each page read is a 9P RPC). The `CQS_SKIP_INTEGRITY_CHECK=1` escape hatch exists, but is undocumented in README and the user has to know it by name. For the WSL-specific case, the check is not earning its cost: a dev-tool search index that can be rebuilt with `cqs index --force` in 10 minutes doesn't need a startup canary that takes 43s on every write open (every `cqs index`, `cqs gc`, `cqs notes add` etc.). The quick_check result isn't even acted upon beyond returning `StoreError::Corruption` — the user then runs `cqs index --force` anyway. The same problem doesn't exist on native Linux or Windows (both manage pages locally; walking 1GB is <1s on SSD).
- **Suggested fix:** On WSL (detected via `is_wsl()`), skip `quick_check` by default. This is the same reasoning as SEC-13 `is_wsl_mount` in `config.rs` (skipping permission checks because NTFS always reports 777). Alternatively, run the check async on a background thread so it doesn't block startup, and only fail the next write attempt if the check eventually finds corruption. At minimum, document `CQS_SKIP_INTEGRITY_CHECK=1` in README as the WSL escape hatch.

## PB-NEW-9: `file_name().to_str()` fallback to hardcoded "splade.index" collapses non-UTF-8 sibling temp files

- **Difficulty:** easy
- **Location:** src/splade/index.rs:284-291
- **Description:** The temp file naming uses:
  ```rust
  let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("splade.index");
  let tmp_path = parent.join(format!(".{}.{:016x}.tmp", file_name, suffix));
  ```
  If the target path has a non-UTF-8 filename (possible on Linux, where paths are `[u8]`, and on Windows where `OsStr` can hold unpaired UTF-16 surrogates), the fallback `"splade.index"` is used. Two concurrent saves targeting different non-UTF-8 filenames in the same directory collide on `.splade.index.{suffix}.tmp`. The `crate::temp_suffix()` adds 64 bits of randomness, so actual collision is astronomically unlikely — but the temp file is no longer uniquely associated with its target, so if both saves race, the cross-contamination is possible (unlikely given the suffix, but the invariant is broken). The HNSW save has the same pattern and the same unlikely-but-broken invariant.
- **Suggested fix:** Use `path.file_name().map(|s| s.to_string_lossy())` instead, which preserves non-UTF-8 bytes as replacement characters but keeps the name distinct. Or derive the temp name from the full target path via `blake3::hash(path.as_os_str().as_encoded_bytes())` truncated to 8 bytes — guaranteed unique per target, no UTF-8 assumption.

## PB-NEW-10: `save_all_sparse_vectors` result-set materialized via `fetch_all` before streaming — spikes RSS on WSL low-memory VMs

- **Difficulty:** easy
- **Location:** src/store/sparse.rs:158-200
- **Description:** `load_all_sparse_vectors()` calls `.fetch_all(&self.pool).await?` then iterates the resulting `Vec<SqliteRow>` to group by chunk_id. For a cqs-sized project with SPLADE-Code 0.6B, this holds ~7.58M `SqliteRow` objects in memory before the first posting is processed — temporary RSS can hit 1-2GB during SPLADE rebuild. On a WSL VM with the common 8GB default (`wslconfig` default is 50% of host RAM, capped at 8GB on older versions), this competes with the ONNX model (~2GB), the embedder cache (~1GB), and the dense HNSW (~1-2GB) all loaded simultaneously by `cqs query`. Indexing a large project in WSL with the default memory ceiling has been observed hitting OOM killer during the SPLADE rebuild path. Peak is transient (~5s) but it matters on low-RAM VMs and on CI. Native Linux/macOS rarely hit this because they have more headroom. RM-6 in batch 1 findings calls this out generally; this is the WSL-specific angle on it.
- **Suggested fix:** Replace `.fetch_all()` with `sqlx::query(...).fetch(&self.pool)` and a `futures::StreamExt::next().await` loop that builds `result` directly. Peak RSS drops to approximately the final index size (~60-100MB). This is almost free from a throughput standpoint because SQLite streams rows to sqlx as it reads them, so `fetch_all`'s "materialize everything" is pure overhead. Alternatively, add a `WHERE token_id IS NOT NULL LIMIT N OFFSET M` pagination loop with N=100000 to bound peak by 100k rows at a time — slower but more predictable.

---

# Security — v1.22.0 audit

Three new findings. SEC-4 (reference `path` containment) and SEC-5 (FTS5 operator) are already tracked and not re-reported. The SPLADE header OOM / missing file-size cap issues are owned by the Robustness agent.

## Reference `source` field is never containment-validated, enabling arbitrary file-read via a checked-in `.cqs.toml`
- **Difficulty:** medium
- **Location:** src/config.rs:293-309 (validate), src/cli/commands/infra/reference.rs:110-111 (add), src/cli/commands/infra/reference.rs:303-380 (update)
- **Description:** `ReferenceConfig` has two path fields: `path` (where the ref index is stored, tracked by the open SEC-4 finding) and `source` (the directory whose files get indexed). `Config::validate` on lines 293-309 runs a containment *warning* on `r.path` only. `r.source` is never checked anywhere — `cmd_ref_add` canonicalizes it on line 110 but does not bound it, and `cmd_ref_update` (line 313-316) just `as_ref().unwrap()`s it straight out of the config and feeds it to `run_index_pipeline`.

  **Attack:** an attacker ships a repo whose root contains:
  ```toml
  [[reference]]
  name  = "rust-std-docs"   # innocent-looking
  path  = "~/.local/share/cqs/refs/rust-std-docs"
  source = "/home/user/.ssh"
  weight = 0.8
  ```
  When the victim clones the repo and later runs `cqs ref update rust-std-docs` (or even `cqs index` if a local flow triggers an update — but just `ref update` is sufficient), `enumerate_files` walks `/home/user/.ssh`, the indexer embeds every text file, and the chunks (including full content) are written into the reference store at `~/.local/share/cqs/refs/rust-std-docs/index.db`. The attacker can later recover the content by shipping any machine with a `cqs --ref rust-std-docs search "BEGIN OPENSSH"`-style query — or just by reading the DB file directly off the filesystem if they have any other foothold.

  This cleanly escalates the semi-trusted reference boundary from "search results can be poisoned" (SECURITY.md current position) to "arbitrary file read into an attacker-readable DB". SEC-4 covers `path`, not `source`, so this is not a duplicate.
- **Suggested fix:** In `Config::validate`, extend the SEC-4 loop to also canonicalize and warn when `r.source` leaves `project ∪ $HOME`. Better: in `cmd_ref_update`, reject any `source` that is not under the same project root as the config file that declared it, unless an explicit `--allow-external-source` flag is passed. At minimum, print `source` prominently (not just name) in the `cqs ref update` confirmation line so a victim sees `Updating reference 'rust-std-docs' (source: /home/user/.ssh)` before work starts.

## `log_routed` creates `telemetry.jsonl` with default umask and no advisory lock, unlike `log_command`
- **Difficulty:** easy
- **Location:** src/cli/telemetry.rs:106-141 vs src/cli/telemetry.rs:30-100
- **Description:** `log_command` (line 88-96) opens the telemetry file via `OpenOptions::append(true).create(true)` and then immediately runs `set_permissions(0o600)`, AND it holds `telemetry.lock` via `lock_file.try_lock()` for the whole write. `log_routed` on lines 137-140 does neither: it uses `OpenOptions::new().create(true).append(true).open(&path)` with no permission set and no lock. Today this is masked because `dispatch.rs:28` calls `log_command` unconditionally before any subcommand runs, so the file is already 0o600 and 0o600 is preserved by subsequent `append` opens. But (a) the pattern is fragile — if a future caller (batch mode handler, test harness) calls `log_routed` without going through `dispatch::run_with`, the file is created world-readable; (b) with no advisory lock, a concurrent `cqs telemetry reset` holding the exclusive lock will happily race with `log_routed` and can end up with a truncated or interleaved line, or a chmod on a file that no longer has the entry the reset thought it had. Recent queries, which `log_routed` records verbatim along with routing confidence and category, are enough to reconstruct what the user is searching for — not a major secret, but the asymmetry between the two loggers is a clear bug.
- **Suggested fix:** Extract the "write one JSON line to telemetry" body into a single private helper used by both `log_command` and `log_routed`. The helper takes the advisory `telemetry.lock`, opens with `0o600` (mode flag on Unix via `OpenOptionsExt`, unconditional `set_permissions` on the parent path as a fallback), appends, flushes. Batch-1 SEC-1/SEC-2 are about the general "default umask then chmod" race; this finding is about one caller not participating in that fix at all.

## `run_git_log_line_range` does not reject absolute paths or `..` components, relying only on store provenance
- **Difficulty:** easy
- **Location:** src/cli/commands/io/blame.rs:69-111
- **Description:** `run_git_log_line_range` validates `rel_file` against a leading `-` (to block `git`'s `--option`-style argument injection) and against an embedded `:` (to block `-L start,end:FILE` misparsing), but it does not check that `rel_file` is actually relative. A stored `chunk.file` of `/etc/passwd` or `../../etc/passwd` would pass both checks and reach `git log -L 1,5:/etc/passwd` as an argument.

  Reachability: `cmd_blame` goes through `resolve_target(&ctx.store, target)` which only queries the *primary* project store, so today the stored `chunk.file` values are produced by cqs's own indexer walking the project — not obviously exploitable. But this is the last line of defense for any future path where the primary store gets content from an untrusted source (reference-index merge, `cqs convert` indexed output, an LLM-summary round-trip, a TOML-imported chunk). Every other path-consuming command in the audit (the `cqs read` command on line 32-40 of read.rs, the `cmd_convert` output check on convert.rs:27-35, the webhelp/CHM walkers) canonicalizes and bounds the path; `run_git_log_line_range` is the one exception, and the comment that justifies its character-level whitelist doesn't explain why absolute-path/`..` are omitted.
- **Suggested fix:** After the `:`/`-` checks, `let p = Path::new(rel_file); if p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) { bail!(...) }`. Cheap, strictly defense-in-depth, and makes the invariant ("rel_file is a project-root-relative path") explicit instead of inherited-by-convention from the indexer.

---

# Data Safety — v1.22.0 audit

Six findings. Core theme: the v1.22.0 SPLADE persistence contract (generation counter in metadata, blake3-hashed file body) is only enforced by `upsert_sparse_vectors` and `prune_orphan_sparse_vectors`. Every other code path that mutates chunks or sparse_vectors bypasses the generation bump, producing persisted SPLADE files that pass the load check but contain stale or orphaned data. Three of the six are variations of that hole; the other three are independent.

## DS-W1: `prune_missing`/`prune_all` delete sparse_vectors in-transaction but never bump `splade_generation`

- **Difficulty:** easy
- **Location:** src/store/chunks/staleness.rs:116-128 (prune_missing), src/store/chunks/staleness.rs:248-260 (prune_all)
- **Description:** The DS-1/DS-6 fix moved orphan `sparse_vectors` deletion into the `prune_missing`/`prune_all` transactions. That deletes rows but does NOT `UPDATE metadata SET value = ... WHERE key = 'splade_generation'`. Process A runs `cqs index --gc` → `prune_all` commits the delete with `splade_generation = 5` still in metadata. The on-disk `splade.index.bin` also has generation = 5 embedded in its header. Next reader (or same process, via `ensure_splade_index`) opens the store, reads generation = 5, loads the persisted file which passes the generation match, and uses an inverted index whose id_map still references the chunks just pruned. The `chunk_type_language_map` filter drops the orphan IDs at read time when it is rebuilt from the fresh store, but (a) the `max_sparse` normalization in `search_hybrid` is computed before the filter, so normalized scores for the surviving results are depressed by the orphan's raw score, and (b) the in-process `chunk_type_map_cache` OnceLock is never invalidated inside a single Store lifetime, so a long-lived `cqs watch` process that ran GC itself still has the pre-prune map and the orphan IDs pass the filter, returning results for deleted chunks.
- **Suggested fix:** After the sparse delete, `if pruned_sparse > 0 { bump splade_generation inside the same transaction }`. Extract a helper `bump_splade_generation(&mut tx)` since three call sites will need it (upsert_sparse_vectors already has the inline version, prune_all, prune_missing).

## DS-W2: Watch-mode reindex writes chunks but never updates sparse_vectors or `splade_generation`

- **Difficulty:** medium
- **Location:** src/cli/watch.rs:684-882 (reindex_files — no SPLADE call anywhere)
- **Description:** `reindex_files` parses changed files, upserts chunks via `upsert_chunks_and_calls`, deletes phantoms via `delete_phantom_chunks`, and upserts type edges — but has zero SPLADE integration. Every file edit in a watch session produces new chunk IDs (id format is `{path}:{line}:{content_hash}`), those new IDs have no rows in `sparse_vectors`, and the old chunk IDs' sparse rows are leaked. `splade_generation` is never bumped. Worse: the persisted `splade.index.bin` file still carries a generation number that matches the unchanged metadata key, so `SpladeIndex::load` succeeds for the next reader and returns an index built from pre-edit data. Sparse-search hit rate silently collapses to near-zero for recently-edited files, and `sparse_vectors` rows leak across the entire watch session. Compounding bug: even if watch were to call `upsert_sparse_vectors`, it has no `SpladeEncoder` wired (no `&Embedder`-style lazy init for the encoder), so the fix is more than a one-line addition.
- **Suggested fix:** Two options, both non-trivial. (1) Wire a `SpladeEncoder` into the watch config, encode the new chunks, and call `upsert_sparse_vectors` after `upsert_chunks_and_calls`. (2) If (1) is too heavy for the watch hot path, at minimum call `delete_sparse_for_chunk_ids(&old_ids)` alongside `delete_phantom_chunks`, and bump `splade_generation` — that downgrades gracefully: reader sees generation mismatch, rebuilds from SQLite, still loses sparse coverage on the new chunks but doesn't return stale results. Either way, add a watch integration test that edits a file mid-session and asserts SPLADE-hybrid search finds the new signature.

## DS-W3: `delete_by_origin` / `delete_phantom_chunks` / `upsert_chunks_and_calls` leak sparse_vectors (no FK cascade)

- **Difficulty:** easy
- **Location:** src/store/chunks/crud.rs:414-436 (delete_by_origin), src/store/chunks/crud.rs:524-588 (delete_phantom_chunks), src/store/chunks/crud.rs:443-517 (upsert_chunks_and_calls)
- **Description:** `sparse_vectors` is declared in schema.sql without a foreign key to `chunks`. `calls` and `type_edges` both use `FOREIGN KEY (...) REFERENCES chunks(id) ON DELETE CASCADE`, so they are cleaned up automatically. `sparse_vectors` has no such constraint, and all three of the above delete/replace code paths forget to delete the corresponding sparse rows manually. Since chunk IDs are content-hash-suffixed, a file edit creates a new chunk ID for any modified function and orphans the old one's sparse rows forever (until a full `cqs index --gc`). Over a long watch session the orphan count grows monotonically and the SPLADE index rebuild time from SQLite inflates proportionally.
- **Suggested fix:** Add `FOREIGN KEY (chunk_id) REFERENCES chunks(id) ON DELETE CASCADE` to `sparse_vectors` in a new v19 migration. `PRAGMA foreign_keys = ON` is already set in `open_with_config` (src/store/mod.rs:348). Requires a v18→v19 migration that recreates `sparse_vectors` with the FK via the `CREATE new / INSERT SELECT / DROP old / RENAME` pattern already used in `migrate_v15_to_v16`, and bumping `splade_generation` at migration time so the persisted index file is invalidated.

## DS-W4: TOCTOU between `splade_generation()` and SpladeIndex save lets Process A save gen-N data labeled as gen-M

- **Difficulty:** medium
- **Location:** src/cli/commands/index/build.rs:512-562 (post-upsert save path), src/cli/store.rs:172-210 (query-time load_or_build), src/cli/batch/mod.rs:281-320 (batch-mode ensure_splade_index)
- **Description:** In three places the sequence is: `(1) read splade_generation from metadata; (2) build or load sparse vectors; (3) persist SpladeIndex with the generation read in step 1`. All three reads are unprotected by any transaction or lock. In `cmd_index` at line 536, `let generation = store.splade_generation().unwrap_or(0)` happens AFTER the upsert transaction has committed (which bumped the value to, say, 6), but BEFORE `idx.save(&splade_path, generation)`. If a concurrent process (watch, another manual index run) performs another write in that window and bumps the generation to 7, Process A writes its still-in-memory gen-6 vectors to disk labeled as gen-6 (correct), but then reads 7 on the next `splade_generation()` call and decides the index is stale — best case, a spurious rebuild. Worse: in `ensure_splade_index` (batch) and `splade_index()` (CLI), the closure inside `load_or_build` reads `load_all_sparse_vectors()` AFTER the generation was captured. A concurrent upsert between the two reads produces a file whose labeled generation is behind the data it actually contains. The persisted file then passes a future load check (header says gen-6) but was built from gen-7 data, and if another upsert bumps to gen-7 naturally, the reader sees "disk == store" and trusts the file — silent staleness.
- **Suggested fix:** Read `splade_generation` from inside the same read transaction as `load_all_sparse_vectors` so the generation snapshot is consistent with the row snapshot. A pooled SQLite connection in WAL mode gives readers a transaction-consistent snapshot across queries if they share one connection; pass a single `PoolConnection` into both queries instead of hitting `&self.pool` twice. Also: guard the persist-from-build path in `cmd_index` by re-reading the generation immediately before `idx.save()` and failing the persist (not the command) if it changed, forcing the next invocation to pay a rebuild instead of writing a mis-labeled file.

## DS-W5: `cqs index --force` renames `index.db` out from under a running `cqs watch`, silently losing every write watch does during the rebuild window

- **Difficulty:** medium
- **Location:** src/cli/commands/index/build.rs:116-167 (force-rebuild rename), src/cli/commands/index/build.rs:609-612 (backup cleanup)
- **Description:** Force rebuild at line 154 does `std::fs::rename(&index_path, &backup_path)` while a concurrent `cqs watch` (running as the systemd service, very common on dev machines) has the same `index.db` opened via sqlx's WAL pool. On Linux/WSL the rename succeeds and the old file handles continue to point at the moved inode. Watch keeps flushing its WAL to the backup file. The rebuild opens a brand-new `index.db`, writes fresh chunks, saves HNSW, succeeds, then at line 611 `remove_file(&backup_path)` deletes the file that watch is actively writing to — or rather, unlinks the last named reference to its inode. Watch's connections still hold the inode alive, but when watch eventually closes the pool (idle timeout, service restart, or process exit), its WAL checkpoint writes to an orphan inode that is then fully garbage-collected by the OS. Every file edit that watch indexed during the rebuild window is lost with no log, no error, no corruption detection. The process-local `WRITE_LOCK` does not help here — it serializes within one process, not across processes, and the rebuild path also uses `Store::open` rather than acquiring any inter-process lock.
- **Suggested fix:** Acquire an exclusive file lock on `index.db.lock` before the rename and keep it for the entire rebuild. `cqs watch` must acquire a shared or exclusive lock on the same file before opening its store, and release-and-reacquire when the lock is broken. Alternatively (cheaper): have `cqs index --force` signal the systemd service to stop, run the rebuild, and restart it — but that requires a documented handoff and is easy to forget. The lock approach is the correct fix.

## DS-W6: Concurrent `check_schema_version` races produce spurious "duplicate column" migration failures

- **Difficulty:** easy
- **Location:** src/store/metadata.rs:23-74 (check_schema_version), src/store/migrations.rs:29-57 (migrate), src/store/migrations.rs:286-295 (migrate_v17_to_v18)
- **Description:** `check_schema_version` reads the current schema_version from `self.pool` (no transaction), then calls `migrations::migrate(&self.pool, from, to)`, which opens its own `pool.begin()` and runs `ALTER TABLE chunks ADD COLUMN embedding_base BLOB`. Two concurrent `cqs` invocations on a fresh v17 DB can both read version=17 before either commits. Process A acquires the exclusive write lock first, runs ALTER TABLE, commits, updates schema_version to 18. Process B then acquires the lock (wait satisfied by busy_timeout) and runs the same ALTER TABLE — which fails with SQLite error "duplicate column name: embedding_base". Process B's entire `Store::open` returns an error, and the user sees "migration failed" on a correctly-migrated database. Same shape applies to v16→v17 (adds `sparse_vectors` table with `IF NOT EXISTS` — survives) and the `ALTER TABLE chunks ADD COLUMN enrichment_version` in that migration (does NOT have IF NOT EXISTS — also crashes on race). Not corruption, but a crash-looking failure on a healthy DB during rapid CI or during `cqs watch` startup colliding with `cqs index`.
- **Suggested fix:** Re-read `schema_version` INSIDE the migration transaction before executing any DDL, and short-circuit if the version was bumped by a concurrent process. Pattern: `let tx = pool.begin().await?; let current: i32 = sqlx::query_as("SELECT value FROM metadata WHERE key='schema_version'").fetch_one(&mut *tx).await?.0.parse()?; if current >= to { tx.rollback(); return Ok(()); }` Then the rest of the migration runs under an exclusive lock held by `tx` from its first write onward. This is the standard "double-check under lock" pattern and costs one extra SELECT per open on a stale-schema DB.

---

# Performance — v1.22.0 audit

11 findings. Mix of big-ticket session/runtime startup amortization, hot-loop per-row allocations in the search scoring path, and SPLADE load-time copies. Cold-start cost (6.9s) and warm SPLADE cost (9.7s) can each likely be cut by 20-40% with the easy/medium items.

## PF-1: No persistent daemon — model + index reloaded every CLI invocation
- **Difficulty:** hard
- **Location:** src/main.rs:14-32, src/cli/dispatch.rs:23
- **Description:** Every `cqs query` spawn pays the full startup tax: tokio runtime build, SQLite pool open, mmap attach, 500MB ONNX session init (~1-3s CPU / 500ms-1s GPU), HNSW file mmap + id_map JSON parse, optional SPLADE 60MB file read + parse. At 6.9s per non-SPLADE query and 9.7s per warm SPLADE query, the agent workflow (typically a burst of 5-20 queries per turn) re-pays this per invocation. A `batch` subcommand exists (src/cli/batch/) that holds state warm, but agents don't use it because it's a REPL mode, not a server. No `cqs daemon` / `cqs serve` that agents could hit via a short-lived socket/IPC call.
- **Suggested fix:** Add `cqs serve` (Unix domain socket or TCP localhost) that keeps `CommandContext` alive across requests. Agents get a thin `cqs` client that opens the socket, serializes the subcommand, reads result. Skip startup amortization entirely for the warm case. Target: <200ms end-to-end for a warm query, vs 6.9s cold. On Windows, fall back to named pipe or LocalHost TCP with file-based auth token.

## PF-2: Embedding cache never hit for query path — only indexing uses it
- **Difficulty:** medium
- **Location:** src/embedder/mod.rs:536-592, src/cache.rs:145-200
- **Description:** `EmbeddingCache` (SQLite, keyed by content_hash + model_fingerprint) is populated only during indexing (`cmd_index`). `embed_query()` uses only an in-memory LRU cache that is destroyed with the Embedder — so every fresh CLI invocation re-tokenizes and re-runs ONNX on the query even when the same query was issued 30 seconds ago. At ~200-500ms per query inference (CPU) or ~50-100ms (GPU), this is pure waste for repeated queries. Agents often re-issue the same search multiple times per session.
- **Suggested fix:** In `embed_query`, after the in-memory LRU miss, compute `blake3(text + query_prefix)` and check the global `EmbeddingCache` with key `(hash, model_fingerprint)`. On hit, decode blob → Embedding. On miss, run inference and write to global cache. Gate behind `CQS_QUERY_CACHE_PERSIST=1` at first to avoid poisoning the global cache with ad-hoc strings; flip on once validated. Expected hit rate: 30-60% for agent workflows that re-query the same natural-language string within minutes.

## PF-3: CAGRA index rebuilt from store every CLI invocation (~100MB data pull)
- **Difficulty:** hard
- **Location:** src/cli/store.rs:253, src/cagra.rs:432-490
- **Description:** The comment on line 215 ("CAGRA rebuilds index each CLI invocation (~1s for 474 vectors)") is stale for this corpus size. At 24,314 vectors × 1024 dim × 4 bytes = 95MB of embedding data pulled from SQLite and shipped to GPU per CLI call — 10k batches × 1024-byte rows through sqlx, then reshape into `Array2<f32>`, then CAGRA graph build. This is why CAGRA's "10x faster search" doesn't show up at CLI granularity: the rebuild dominates the per-query cost. At ~5000 threshold (configurable), this triggers for realistic corpora but is unhelpful for single-shot searches. Without a persistent daemon (PF-1), CAGRA is a net slowdown vs plain HNSW for interactive use.
- **Suggested fix:** Two options. (a) Raise `CQS_CAGRA_THRESHOLD` default from 5000 to 200,000 (only rebuild when HNSW traversal is genuinely slower than the rebuild cost). (b) Better: persist the CAGRA graph to disk alongside HNSW (similar to PR #895 SPLADE persistence). CAGRA has a build-time graph we can serialize — only the cuVS resources need fresh per-invocation. (c) Short-term: skip CAGRA entirely when the only call will be one search, and only rebuild when batching 5+ queries (detectable via CLI subcommand or `cqs batch`).

## PF-4: SPLADE index body read into Vec<u8> without capacity hint
- **Difficulty:** easy
- **Location:** src/splade/index.rs:395-396
- **Description:** `let mut body = Vec::new(); reader.read_to_end(&mut body)?;` — for a 59MB SPLADE body, this causes ~log2(59MB) Vec reallocations (each doubling), each copying the accumulated data. At 59MB that's ~60MB of wasted copy work (~100ms at 600MB/s). The file size is known via `std::fs::metadata(path)?.len()` minus the 64-byte header.
- **Suggested fix:** `let body_size = file.metadata()?.len().saturating_sub(SPLADE_INDEX_HEADER_LEN as u64) as usize; let mut body = Vec::with_capacity(body_size); reader.read_to_end(&mut body)?;`. Saves ~100ms on the 9.7s warm SPLADE query time.

## PF-5: SPLADE index load parses ~24k chunk IDs via `.to_string()` per row
- **Difficulty:** easy
- **Location:** src/splade/index.rs:411, 434
- **Description:** In the load body parser, every chunk ID is extracted via `std::str::from_utf8(&body[..len])?.to_string()`. For 24k chunks that's 24k String allocations, each copying bytes out of the `body` buffer — and the body itself is freed when the function returns. Since the body Vec lives for the duration of the function, the id_map Strings could be built as `String::from_utf8_unchecked(body[..len].to_vec())` or, better, the id_map Vec could be pre-allocated and the body could be consumed byte-by-byte into Strings sharing the same allocation backing.
- **Suggested fix:** Pre-allocate `id_map = Vec::with_capacity(chunk_count_usize)` (already done). Replace `.to_string()` with `String::from_utf8(body[cursor..cursor+len].to_vec())` — shaves only one intermediate &str creation but is clearer. Bigger win: memmap the file with `memmap2::Mmap` and keep the id_map as `Vec<Box<str>>` pointing into the mmap (but that requires self-referential lifetime management). Easier first pass: pre-allocate the String buffer with `String::with_capacity(len)` before push_str. Saves ~20ms on 9.7s warm SPLADE query.

## PF-6: Hot-loop `name.to_lowercase()` per candidate in NameMatcher::score
- **Difficulty:** easy
- **Location:** src/search/scoring/name_match.rs:94, 121-124
- **Description:** In brute-force scoring (cursor-batched over all 24k chunks when no index is used) and in `search_by_candidate_ids` (up to ~500 top candidates for hybrid search), `NameMatcher::score` allocates a fresh `name_lower: String` per candidate via `name.to_lowercase()`, and then tokenizes & lowercases each token into a fresh `Vec<String>` via `tokenize_identifier(name).map(|w| w.to_lowercase()).collect()`. On 24k brute-force scoring that's ~24k heap allocations for the name_lower alone, plus 24k Vec<String> allocations of ~5 items each for name_words. Estimated cost: ~2-5ms per query from allocator pressure and memcpy, but mostly invisible because the embedding cosine dominates. Still low-hanging when scoring cost matters (non-index brute-force fallback).
- **Suggested fix:** Reuse a thread-local `String` buffer via `SmallString` / `smartstring`, or pass a `&mut String` scratch buffer into `score()`. Better: restructure `score()` to avoid the allocation entirely — all the match operations (contains, exact, word overlap) can work with `str::to_ascii_lowercase()` into a stack-allocated SmallVec for identifier names (which are almost always <32 bytes). Alternatively, since chunk names are ASCII-lowercase for most queries that matter, add a fast path: if `self.query_lower.chars().all(|c| c.is_ascii())` and `name.chars().all(|c| c.is_ascii())`, use `eq_ignore_ascii_case` + `str::find` directly.

## PF-7: search_by_candidate_ids allocates lowercased strings per row despite pre-lowercased sets
- **Difficulty:** easy
- **Location:** src/search/query.rs:691-717
- **Description:** Lines 694-698 correctly pre-lowercase `lang_set` and `type_set` once at function entry. But lines 707 and 713 then call `candidate.language.to_lowercase()` and `candidate.chunk_type.to_lowercase()` per candidate row — each a fresh String allocation. For 500 candidates (limit * 5) with include_types filter active, that's 500 String allocations per search. The database already stores these values in canonical lowercase (Language::to_string / ChunkType::to_string return lowercase), so the `.to_lowercase()` is defensive against a format that never happens.
- **Suggested fix:** Replace `!langs.contains(&candidate.language.to_lowercase())` with `!langs.iter().any(|l| l.eq_ignore_ascii_case(&candidate.language))` — zero-alloc, one pass, works even if DB values are ever mixed case. Or better, trust the DB canonical form and do `!langs.contains(candidate.language.as_str())`. Eliminates ~500-1000 String allocations per hybrid search.

## PF-8: finalize_results rebuilds glob matcher for FTS path filtering
- **Difficulty:** easy
- **Location:** src/search/query.rs:306-317
- **Description:** The outer `search_filtered_with_notes` already called `compile_glob_filter(filter.path_pattern.as_ref())` at line 159 for the brute-force loop. `finalize_results` then receives `path_pattern: Option<&str>` and, at line 306-307, re-compiles the glob via `compile_glob_filter(path_owned.as_ref())` — same pattern, second compile. Globset compilation isn't free (builds a regex automaton per pattern); doing it twice is wasteful. For the RRF path it's even worse because the glob is only used here for filtering FTS rows.
- **Suggested fix:** Pass the compiled `GlobMatcher` (not the raw `&str`) through `finalize_results`. Or make `compile_glob_filter` memoize on the pattern string. Skip hasn't been identified as a hot-path issue alone but it's a pattern for other re-compilations.

## PF-9: HashMap::entry + clone in apply_parent_boost hot loop
- **Difficulty:** easy
- **Location:** src/search/scoring/candidate.rs:59-63
- **Description:** `for r in results.iter() { if let Some(ref ptn) = r.chunk.parent_type_name { *parent_counts.entry(ptn.clone()).or_insert(0) += 1; }}`. Every occurrence calls `ptn.clone()` (allocates a fresh String heap copy) even when the entry already exists — `HashMap::entry` takes owned keys, so the first insertion must allocate but subsequent `entry(existing_key)` calls still hash-and-clone. With N results (typically 10-50), that's one clone per result that has a parent_type_name. Minor by itself but it's a repeat pattern across scoring.
- **Suggested fix:** Use the `raw_entry_mut` API (stable) or switch to `parent_counts.get_mut(ptn)` first and only `insert(ptn.clone(), 1)` on miss. Alternatively, use `&str` keys: `HashMap<&str, usize>` borrowing from `r.chunk.parent_type_name`. No allocations at all.

## PF-10: tokio runtime construction on every CLI invocation is measurable for sub-second commands
- **Difficulty:** medium
- **Location:** src/store/mod.rs:333-342, src/cache.rs:67-70
- **Description:** `tokio::runtime::Builder::new_current_thread().enable_all().build()` takes ~5-20ms depending on OS (creates epoll/kqueue/IOCP reactor, starts driver thread, initializes signal handler). For `cqs query` at 6.9s total this is under 0.3%, but for fast commands like `cqs callers foo`, `cqs explain foo`, or `cqs notes list --json` that complete in ~200-500ms, the runtime cost is 2-5% of total. On Windows/WSL the IOCP driver init is slower (~15-30ms). Two runtimes are built per invocation if the command touches the cache (store runtime + cache runtime), doubling the cost for commands that exercise both.
- **Suggested fix:** Share a single `tokio::runtime::Runtime` across Store and EmbeddingCache — stash it in a `static OnceLock<Arc<Runtime>>` lazily initialized on first open. The read-only store already uses `new_current_thread` so the compatibility matrix is simple. In a persistent daemon (PF-1), this is moot, but until then a shared runtime saves ~10ms per invocation and halves cache-touching-command overhead.

## PF-11: read_to_end for SPLADE without mmap prevents OS page cache reuse
- **Difficulty:** medium
- **Location:** src/splade/index.rs:341-490
- **Description:** My hint context said "Mmap would avoid the copy. Is mmap feasible here, or would it cost too much setup per CLI invocation?" — answer: mmap is feasible and would help. The SPLADE file (~59MB) is persistent, so mmap'ing it with `memmap2::Mmap::map(&file)` costs ~10µs (a page-table update), whereas the current read_to_end copies 59MB into user-space heap. On second+ CLI invocation, the OS page cache has the file warm, so mmap reads are just memcpy from page cache to process VM — essentially free. But the current code re-reads the entire 59MB into a new heap buffer every time, bypassing the page cache benefit. Additionally, the current parser copies every chunk ID out of the body buffer into a fresh `String` — with mmap we could parse into a `Vec<&[u8]>` view and only allocate Strings on demand, or keep `id_map: Vec<Range<usize>>` plus the mmap alive.
- **Suggested fix:** Replace the read_to_end branch with `let mmap = unsafe { memmap2::Mmap::map(&file)? };` then parse directly from `&mmap[HEADER..]`. Keep the mmap alive in the SpladeIndex struct (add `_mmap: Option<memmap2::Mmap>`). For the id_map, the simplest correct form is to clone Strings out of the mmap (same as today, no worse), but the big win is avoiding the 59MB heap allocation + 59MB memcpy + 59MB zeroing. Saves ~50-150ms on warm SPLADE query. Risk: mmap'd files can segfault if the file is truncated under us — mitigate by holding a shared file lock (already done for HNSW) during the lifetime of the SpladeIndex.

## Already known / reported elsewhere (not re-counted)

- CHUNK_INSERT_BATCH=49 (SHL reported in batch 1).
- Hardcoded timeouts, buffer sizes, misc magic numbers (batch 1).
- cmd_index re-encodes all SPLADE every run (CQ-4 batch 1).
- SPLADE encode_batch + session serialization (PF-5 in v1.19.0 triage, was issue #843, unclear if resolved).
- PF-3/4/6/9/10 from v1.20.0 triage still deferred (bfs_expand clones, compute_risk_and_tests N reverse_bfs, etc.) — unchanged.

---

# Resource Management — v1.22.0 audit

7 findings. Main themes: model-file fingerprinting does a 1.3 GB heap read where a streaming hash would do, `cqs ref list`/`doctor` open reference stores read-write (triggering quick_check per ref), batch-mode cache invalidation forgets `splade_index`, `SpladeIndex::save` leaks orphan temp files on crash, SPLADE persist doubles memory during save, watch mode re-opens `Store::open` (4-thread runtime) after every reindex, and `cqs batch`/`cqs chat` open the primary store read-write despite being a single-thread consumer.

## `Embedder::model_fingerprint` reads the entire ONNX file into a `Vec<u8>` instead of streaming a hash
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:341-363
- **Description:** On first call, `model_fingerprint()` computes `blake3::hash(&bytes)` over `std::fs::read(model_path)`. The 2 GB guard at line 324 only triggers above `2 * 1024^3` bytes, so BGE-large (1.34 GB), SPLADE-Code 0.6B (~540 MB), code-reranker-v1 (~90 MB), and every other sub-2 GB ONNX model force a full heap allocation of the model file just to hash it. That is a transient ~1.3 GB peak RSS spike during `cqs index` — on top of the ORT session mmap of the same file. The correct pattern is already in use elsewhere in the codebase: `hnsw/persist.rs:298-306` uses `blake3::Hasher::new(); hasher.update_reader(file)` for the HNSW checksum, which streams the file through an 8 KB buffer. Impact grows with model size and fires once per index pipeline run (model_fingerprint is wrapped in `OnceLock<String>` so it amortizes within a single cqs invocation, but every `cqs index` or `cqs ref update` pays the spike).
- **Suggested fix:** Replace `std::fs::read(model_path)` with the streaming pattern: `let file = std::fs::File::open(model_path)?; let mut hasher = blake3::Hasher::new(); hasher.update_reader(file)?; let hash = hasher.finalize().to_hex().to_string();`. Also drop the 2 GB special case — the streaming path has constant memory regardless of file size, so the metadata-only fallback at line 324-340 becomes unnecessary.

## `cqs ref list` / `cqs doctor` open each reference store read-write, paying quick_check + 4-thread runtime per reference
- **Difficulty:** easy
- **Location:** src/cli/commands/infra/reference.rs:200, :234, :330 ; src/cli/commands/infra/doctor.rs:243
- **Description:** Four read-only probe sites call `Store::open(&db_path)` (the read-write opener) just to read `chunk_count()` / `stats()`. `Store::open` builds a multi-thread tokio runtime with `worker_threads = 4`, a 4-connection SQLite pool, AND runs `PRAGMA quick_check(1)` (src/store/mod.rs:431-441) on every open — see the comment at line 411-429 describing how this path used to take 85 s on a 1.1 GB index. With N references, `cqs ref list` runs N independent quick_checks, spawns ~5N OS threads, and allocates 4N connection slots. `load_single_reference` at src/reference.rs:93 already uses `Store::open_readonly` (1 connection, current-thread runtime, no integrity check); the ref-list and doctor paths missed this treatment. For reference paths on WSL `/mnt/c/` or NFS the quick_check tax is especially painful.
- **Suggested fix:** Replace the four `Store::open(...)` call sites with `Store::open_readonly(...)`. All four are read-only (`chunk_count`, `stats`, `stored_model_name` via `store.stored_model_name()` in doctor are all read paths). This drops the 5N thread + 4N connection + N quick_check cost down to N + N + 0.

## `BatchContext::invalidate_mutable_caches` forgets `splade_index` — stale 60-100 MB posting map survives reindex
- **Difficulty:** easy
- **Location:** src/cli/batch/mod.rs:178-186, src/cli/batch/mod.rs:89
- **Description:** `invalidate_mutable_caches` clears `hnsw`, `call_graph`, `test_chunks`, `file_set`, `notes_cache`, and the `refs` LRU. It does NOT touch `splade_index: RefCell<Option<SpladeIndex>>`. When `check_index_staleness` detects an index.db mtime change (concurrent `cqs index` writing new sparse vectors), everything else gets dropped and the store re-opened, but the SPLADE index stays pinned — the full `HashMap<u32, Vec<(usize, f32)>>` postings (~60-100 MB on the cqs corpus with SPLADE-Code 0.6B) plus `Vec<String>` id_map (~10 MB at 100k chunks) remain in memory with stale chunk IDs. The next `cqs search` in the same batch session uses the stale index, producing ghost results, AND peak RSS in the batch session is now `(old_splade_index + new_splade_index)` once `ensure_splade_index` rebuilds. The `ensure_splade_index` guard at line 283 (`if self.splade_index.borrow().is_some() { return; }`) means a stale index is never replaced until something explicitly clears it.
- **Suggested fix:** Add `*self.splade_index.borrow_mut() = None;` inside `invalidate_mutable_caches` alongside the other RefCell resets. The next `ensure_splade_index` call after invalidation will rebuild from the fresh on-disk file (or SQLite) using the new generation counter.

## `SpladeIndex::save` leaks orphan `.splade.index.bin.*.tmp` files on crash — no startup cleanup
- **Difficulty:** easy
- **Location:** src/splade/index.rs:284-325, contrast src/hnsw/persist.rs:498-510
- **Description:** `SpladeIndex::save` writes body to a randomized-suffix temp file `.splade.index.bin.{suffix}.tmp` and atomically renames. If the process is SIGKILLed, panics, or hits a disk-full error between file creation and rename, the `.tmp` file (up to ~100 MB) is orphaned in `.cqs/`. There is no equivalent of `HnswIndex::load_with_dim`'s cleanup loop (persist.rs:498-510 scans the directory for stale `.{basename}.*.tmp` entries on load and removes them). Over time, repeated crashes or interrupted `cqs index` runs accumulate orphan files in `.cqs/` that nothing ever deletes. A user hitting OOM killer mid-reindex will pay this storage cost silently. Pattern also applies to the `SpladeIndex::save` path being called from `load_or_build` (line 525), which makes the orphan risk multiply across every generation bump.
- **Suggested fix:** Add a `cleanup_stale_temps` helper called at the top of `SpladeIndex::load` (before `File::open`) that mirrors the HNSW pattern: `read_dir(parent)` → filter `entry.file_name().starts_with(".splade.index.bin.") && entry.file_name().ends_with(".tmp")` → `remove_file`. Or, cleaner, centralize the logic in a `cleanup_stale_index_temps(dir: &Path, basename: &str)` helper shared with HNSW.

## `SpladeIndex::save` holds body Vec and in-memory postings/id_map simultaneously — ~2× memory during persist
- **Difficulty:** medium
- **Location:** src/splade/index.rs:218-262
- **Description:** The save path builds the entire serialized body into `body: Vec<u8>` with `estimate_body_size()` capacity (line 220-223), then walks id_map + postings HashMap to fill it (lines 226-261), then calls `blake3::hash(&body)` (line 264) and finally `writer.write_all(&body)` (line 312). During this sequence, the in-memory `SpladeIndex` (postings HashMap + id_map Vec) and the serialized-body Vec both exist. The doc comment at line 204-207 admits "the body is built in memory (~60-100MB for SPLADE-Code 0.6B on a cqs-sized project) … no new budget is introduced" — but that is wrong: it IS a new budget because it doubles the SPLADE memory footprint for the duration of the save. On a 500 k-chunk corpus this could be ~300-500 MB transient heap on top of the already-held in-memory index. The pattern is load-bearing because `SpladeIndex::save` runs inside `load_or_build` on every first invocation after a reindex, precisely when memory is already taxed by `cqs index`.
- **Suggested fix:** Stream the body directly to a `BufWriter` wrapping the temp file with a `blake3::Hasher` tee: write header placeholder (64 zeros), then for each emitted field do `writer.write_all(&bytes)?; hasher.update(&bytes);`, finalize hash, `file.seek(0)`, write the real header over the placeholder, `sync_all`. This drops the `body: Vec<u8>` allocation entirely — steady-state is the BufWriter's ~8 KB buffer. Use the same pattern as HNSW checksum computation at persist.rs:298-306.

## `cqs watch` re-opens `Store::open` after every reindex cycle, spawning a fresh 4-thread tokio runtime each time
- **Difficulty:** medium
- **Location:** src/cli/watch.rs:384-387, src/store/mod.rs:338-342
- **Description:** `run_watch` opens the store once at line 296, then after each reindex cycle drops and re-opens it at 384-387 ("DS-9: Re-open Store to clear stale OnceLock caches"). Each open creates a new `tokio::runtime::Builder::new_multi_thread().worker_threads(4)` runtime. Over a long-lived systemd watch session (24/7 service) with many reindex cycles, this churn creates and destroys runtimes hundreds of times per day. Each runtime startup spawns 4 worker threads + an io-driver thread, runs quick_check(1) on the database (85 s initially on large DBs — recently changed from integrity_check in PR #893 but quick_check still walks the B-tree), and re-computes the dim lookup. The runtime reuse pattern is available: after `prune_missing` + HNSW save, the OnceLock caches could be reset via a `store.clear_caches()` method rather than re-opening the entire Store. Plus, since watch mode is strictly single-threaded, the 4-worker multi-thread runtime is over-provisioned.
- **Suggested fix:** Two independent improvements: (1) Add `pub fn clear_onetime_caches(&self)` on `Store` that resets the `OnceLock<Arc<CallGraph>>`, `OnceLock<Arc<Vec<ChunkSummary>>>`, and chunk_type_language_map caches without tearing down the pool or runtime. Watch mode calls this instead of drop+re-open at line 384. (2) Use `Store::open_readonly_pooled` for watch mode's main store — the reindex pipeline already opens its own writable store via `run_index_pipeline` (pipeline/mod.rs), and watch's long-lived store holds the file for staleness checks and notes reads. If the pipeline path needs write access, open a short-lived writable store only during the reindex cycle.

## `cqs batch` / `cqs chat` open the primary store read-write even though most commands are read-only — 4-thread runtime for a single-stdin consumer
- **Difficulty:** medium
- **Location:** src/cli/batch/mod.rs:578, src/cli/batch/mod.rs:154, src/cli/batch/mod.rs:195, src/store/mod.rs:275-286
- **Description:** `create_context()` calls `open_project_store()` which resolves to `Store::open` (read-write). This spins up a 4-thread tokio runtime + 4-connection pool for a batch session whose stdin pipeline is strictly single-consumer. `check_index_staleness` at line 154 and `invalidate` at line 195 also use `Store::open` on re-open. The batch handlers in `src/cli/batch/handlers/` are read-only for everything except the `audit` command (which toggles audit state in metadata), `refresh` (cache invalidation, no DB write), and there is no `notes add` handler. Meanwhile `CommandContext::open_readonly` is the right shape for single-thread readers and already exists (src/cli/store.rs:60). The RW open also pays `PRAGMA quick_check(1)` on every batch session startup — typically ~1-3 s on a warm FS cache.
- **Suggested fix:** Switch `create_context` (and the staleness re-open path at line 154) to use `open_project_store_readonly()` by default. For the audit-toggle command, have the `audit` handler internally do a scoped `Store::open(&index_path)` write, mutate, drop. That restricts the 4-thread runtime to the brief write window. Alternatively, add a `Store::open_with_config` preset "batch_mode_primary" that uses `use_current_thread: true` and `max_connections: 1` but keeps `read_only: false` so the rare audit-toggle works in-place.

---

# Test Coverage (Happy Path) — v1.22.0 audit

14 happy-path gaps: untested SPLADE persistence integration surfaces, untested sparse store getters, missing-from-invalidation SPLADE cache, and a recurring pattern of struct-serialization-only tests that never exercise the orchestration function they sit next to.

## [CommandContext::splade_index — zero tests for PR #895's main entry point]
- **Difficulty:** medium
- **Location:** src/cli/store.rs:172 (the load/fallback/persist orchestrator)
- **Description:** PR #895 added lazy SPLADE persistence as the hot path for every `cqs search --splade`. No test exercises the lazy loader end-to-end. A regression that breaks the closure-to-build flow (e.g. early-return on empty generation, or using the wrong cqs_dir) would silently force SQLite rebuild on every invocation — the exact 45s-per-query regression the PR fixed — and no test catches it. The `rebuilt` boolean returned by `load_or_build` is observable, but no integration test asserts "second call returns `rebuilt == false` after first call persisted". The `splade.index.bin` round-trip through CommandContext is untested.
- **Suggested fix:** `test_command_context_splade_index_persists_and_reloads` — construct a CommandContext against a test store with sparse_vectors already written, call `splade_index()` once (expect rebuild), assert `splade.index.bin` exists in cqs_dir, construct a fresh CommandContext on the same cqs_dir, call `splade_index()` again, assert the SpladeIndex length matches. A second test bumps generation between the two constructions and asserts the second load rebuilds (same pattern as splade::index::test_load_or_build_persists_on_first_call, but through the user-facing entry point).

## [BatchContext::ensure_splade_index — zero tests, zero staleness-invalidation test]
- **Difficulty:** medium
- **Location:** src/cli/batch/mod.rs:281 (the batch-mode lazy loader) and src/cli/batch/mod.rs:178 (invalidate_mutable_caches)
- **Description:** `ensure_splade_index` is called by every batch/chat search — it's the hotter of the two lazy loaders. Zero direct tests. More importantly, `invalidate_mutable_caches` at line 178 clears `hnsw`, `call_graph`, `test_chunks`, `file_set`, `notes_cache`, and `refs` — but does NOT clear `splade_index`. This is a **real bug**: a concurrent `cqs index` bumps the index.db mtime and `splade_generation`, the batch session detects mtime staleness and invalidates everything else, but `splade_index` keeps the pre-reindex posting list and all subsequent batch searches return stale results until the chat session is restarted. The existing `test_invalidate_clears_mutable_caches` asserts `file_set/notes_cache/call_graph/test_chunks/hnsw` are cleared — but not `splade_index`, which is why this bug hasn't been caught.
- **Suggested fix:** Two tests in src/cli/batch/mod.rs tests module: (1) `test_invalidate_clears_splade_index` — populate `ctx.splade_index` with a dummy SpladeIndex, call `ctx.invalidate()`, assert `ctx.splade_index.borrow().is_none()`. This test will FAIL against current code and surface the real invalidation bug. (2) `test_ensure_splade_index_reloads_after_generation_bump` — end-to-end: pre-populate sparse_vectors, call `ensure_splade_index()`, touch the store to bump generation, call `check_index_staleness()`, call `ensure_splade_index()` again, assert the new index reflects the updated sparse data.

## [load_all_sparse_vectors — only tested via 2-chunk roundtrip, misses ORDER BY contract]
- **Difficulty:** easy
- **Location:** src/store/sparse.rs:158 (the grouping accumulator)
- **Description:** The SQLite query uses `ORDER BY chunk_id` to enable single-pass grouping — if ORDER BY is removed or a new index ordering is introduced, the grouping logic at lines 172-191 would silently fragment a single chunk's vectors into multiple entries (`current_id.as_ref() != Some(&chunk_id)` check fails across non-adjacent rows). The existing `test_sparse_roundtrip` inserts 2 chunks, so the fragmentation bug is invisible. This function is the fallback builder for every `load_or_build` miss — a silent mis-group corrupts the SPLADE index in both disk persistence AND in-memory build paths.
- **Suggested fix:** `test_load_all_sparse_vectors_groups_rows_correctly` — upsert 5 chunks each with 3-4 distinct token_ids, load via `load_all_sparse_vectors`, assert each chunk's returned SparseVector has exactly the count of token_ids its upsert included, and the chunk order is stable across runs. A stronger variant: `test_load_all_sparse_vectors_interleaved_insertion_order` — insert rows in a non-sorted chunk_id order (directly via SQL bypassing upsert_sparse_vectors which batches), call load, verify grouping is still correct.

## [Store::chunk_splade_texts — zero tests, load-bearing text concatenation invariant]
- **Difficulty:** easy
- **Location:** src/store/sparse.rs:204 (the `name + sig + doc` concatenation)
- **Description:** `chunk_splade_texts` produces the text that gets encoded into sparse vectors for every chunk. The concatenation rule "if doc is non-empty: name + sig + doc; else: name + sig" is defined in 3 lines of Rust. A regression that swaps to "sig + name + doc" or drops the doc would silently change the token distribution for all future index builds, without any failing test. The function has ZERO tests and exactly one caller (build.rs line 402) that passes the result straight to `encode_batch`.
- **Suggested fix:** `test_chunk_splade_texts_concatenation_format` — insert one chunk with {name="foo", signature="fn foo()", doc=Some("does things")}, call `chunk_splade_texts`, assert result == `[("chunk_id", "foo fn foo() does things")]`. Companion `test_chunk_splade_texts_empty_doc_omits_doc_field` — same but with `doc=None`, assert result == `[("chunk_id", "foo fn foo()")]`. Third: `test_chunk_splade_texts_empty_string_doc_treated_as_missing` — verifies the `Some(d) if !d.is_empty()` branch — insert chunk with `doc=Some("")`, assert the empty doc is NOT appended to the text.

## [Store::get_chunk_ids_and_embeddings_by_hashes — zero tests, HNSW-insert hot path]
- **Difficulty:** easy
- **Location:** src/store/chunks/embeddings.rs:65 (the incremental HNSW update path)
- **Description:** `get_chunk_ids_and_embeddings_by_hashes` is called by watch mode (`src/cli/watch.rs:611`) to assemble (chunk_id, Embedding) pairs for HNSW `insert_batch`. The sibling `get_embeddings_by_hashes` has `test_get_embeddings_by_hashes_single` and `test_get_embeddings_by_hashes` covering it. The chunk-id version has ZERO tests — the batch-size loop (500 hashes per SQL query) and the grouping logic are structurally identical but untested. A regression that changes the SELECT column order (line 86: `SELECT id, embedding`) would silently put embeddings at index 0 and chunk IDs at index 1, corrupting every watch-mode incremental update. The NaN drop-on-error behavior (line 102) is also untested on this entry point.
- **Suggested fix:** `test_get_chunk_ids_and_embeddings_by_hashes_roundtrip` — insert 3 chunks with distinct embeddings and content_hashes, call the function with all 3 hashes, assert the returned Vec is length 3 and each (id, embedding) pair matches the inserted chunk. Companion `test_get_chunk_ids_and_embeddings_by_hashes_batch_boundary` — insert 600 chunks (crosses the 500 batch boundary), assert all 600 round-trip correctly, proving the chunks() iterator preserves per-batch grouping. Parallel to the existing `test_get_embeddings_by_hashes` at tests/store_test.rs:224.

## [cli/commands/review/diff_review.rs::apply_token_budget — zero tests for token math]
- **Difficulty:** easy
- **Location:** src/cli/commands/review/diff_review.rs:78 (the token budget + truncation logic)
- **Description:** `apply_token_budget` computes per-item token estimates (callers=15, tests=18, function=12, note=20, BASE_OVERHEAD=30, JSON_OVERHEAD_PER_RESULT=35), then truncates callers and tests to fit within the budget. It's the ONLY path token-budgeted users hit through `cqs review --tokens N` and `cqs ci --tokens N`. If any constant drifts or the 2/3 caller-budget split (line 104) inverts, the output is silently wrong and users either OOM their context window or lose high-value callers to the "min 1" floor. Zero tests. Same pattern duplicated in ci.rs:68, also untested.
- **Suggested fix:** Three tests in diff_review.rs: (1) `test_apply_token_budget_preserves_all_when_fits` — 2 changed fns + 3 callers + 2 tests → budget=1000 → all preserved. (2) `test_apply_token_budget_truncates_callers_and_tests_proportionally` — 10 changed fns + 50 callers + 50 tests → budget=200 → asserts at least 1 caller + 1 test kept (the documented invariant), asserts total used <= budget. (3) `test_apply_token_budget_warning_appended_on_truncation` — forces truncation, asserts `review.warnings` contains the "Output truncated" message with the right counts. Parallel tests in ci.rs for `apply_ci_token_budget`.

## [cli/commands/graph/test_map.rs::build_test_map — zero tests for BFS core]
- **Difficulty:** medium
- **Location:** src/cli/commands/graph/test_map.rs:80 (reverse BFS + chain reconstruction)
- **Description:** `build_test_map` is a 76-line reverse-BFS with node cap (CQS_TEST_MAP_MAX_NODES), chain walk (chain_limit = max_depth + 1), and dead-end detection. It's called by both the CLI `cmd_test_map` and the batch `dispatch_test_map` — the load-bearing core of `cqs test-map`. The only tests in this file are `test_test_map_output_field_names` and `test_test_map_output_empty` — both serialize a hand-built TestMapOutput struct and don't touch `build_test_map` at all. A regression that breaks the reverse BFS (e.g. reversed forward/reverse map, chain walk hitting the wrong node) would break `cqs test-map` for every user without any failing test. AC-10 (node-cap introduction) shipped with no test for the cap either.
- **Suggested fix:** Five tests in a new test module: (1) `test_build_test_map_single_level_test_caller` — a test calls target directly → returned with depth=1 + chain=[target, test]. (2) `test_build_test_map_transitive_caller` — test → helper → target, depth=2 + chain reflects the 3 nodes. (3) `test_build_test_map_respects_max_depth` — chain of 4 callers with max_depth=2 → only depth-2 ancestors found. (4) `test_build_test_map_node_cap_returns_partial` — set CQS_TEST_MAP_MAX_NODES=5, build a dense graph, assert partial results returned without panic. (5) `test_build_test_map_non_test_chunks_ignored` — an ordinary (non-test) chunk in the ancestor chain is excluded from results.

## [cli/commands/io/brief.rs::build_brief_data — zero tests for test-count BFS]
- **Difficulty:** easy
- **Location:** src/cli/commands/io/brief.rs:37 (the function-to-test-count reverse BFS at depth 5)
- **Description:** `build_brief_data` is called by `cmd_brief` to produce the summary for `cqs brief <file>`. It dedups chunks by name, loads caller_counts batch, and runs a depth-5 reverse BFS per chunk to count test ancestors. The only tests in brief.rs (`brief_entry_serializes_correctly` / `brief_output_serialization`) verify the manual struct serialization — they don't touch `build_brief_data`. A regression that breaks the dedup (moves the `.filter(|c| seen.insert(c.name.clone()))` call), or inverts the depth-5 BFS condition, or miscounts `!=` vs `==` for the `t.name != chunk.name` self-filter at line 100, produces silently wrong brief output. Zero coverage.
- **Suggested fix:** Three tests: (1) `test_build_brief_data_returns_chunks_deduped_by_name` — insert 3 windowed chunks with the same name, assert result has 1 chunk. (2) `test_build_brief_data_caller_counts_populated` — insert chunk + one caller, assert caller_counts returns {name → 1}. (3) `test_build_brief_data_test_counts_traces_depth_5` — chain: test5 → helper4 → helper3 → helper2 → helper1 → target, assert test_counts[target] includes test5 (depth 5 is included per `if depth >= 5 continue` — off-by-one risk). Use the existing `TestStore` helper.

## [cli/commands/search/query.rs::resolve_parent_context — 100-line fn with security boundary, zero tests]
- **Difficulty:** medium
- **Location:** src/cli/commands/search/query.rs:754 (parent lookup + file read with traversal check)
- **Description:** `resolve_parent_context` is called on every `cqs search --expand`. It has two branches: (a) parent-in-DB (normal path), (b) parent-not-in-DB, read source file with path-escape validation at lines 814-826 (RT-FS-1 fix). Zero happy-path test for either branch. The dedup cache at line 795 was specifically CQ-7-fixed in v1.20.0 to use parent_id instead of child_id — but no test verifies the cache hit path. The path-escape guard has no test that an in-root path IS accepted AND the content is correctly sliced from line_start..line_end. A regression in the line slicing (e.g. `start = line_start as usize` without the `saturating_sub(1)`) shifts every expanded result by one line.
- **Suggested fix:** Three tests in a new query.rs test module: (1) `test_resolve_parent_context_parent_in_db` — insert child chunk + parent chunk with known content, assert the returned ParentContext matches the parent's content/line_start/line_end. (2) `test_resolve_parent_context_fallback_reads_source_file` — write a source file to a tempdir, index a chunk pointing to lines 5-10, assert the returned ParentContext content equals lines 5-10 of the file. (3) `test_resolve_parent_context_dedup_uses_parent_id` — two children with the same parent_id, assert parent is resolved exactly once (cache hit) — set up a mock that fails on second DB fetch to prove the cache fires. (4) `test_resolve_parent_context_path_escape_rejected` — insert a chunk with `chunk.file = "../../etc/passwd"`, assert it's skipped (no ParentContext returned) and no file is read.

## [Batch handlers/analysis.rs::dispatch_review/dispatch_ci — six handlers with zero tests]
- **Difficulty:** easy
- **Location:** src/cli/batch/handlers/analysis.rs:23 (dispatch_dead), 58 (dispatch_stale), 76 (dispatch_health), 86 (dispatch_suggest), 134 (dispatch_review), 168 (dispatch_ci)
- **Description:** All six batch-mode dispatchers for analysis commands have ZERO tests. `dispatch_review` and `dispatch_ci` apply token budgets via `apply_token_budget_public` / `apply_ci_token_budget` — the token_budget key is then injected into the output JSON at line 158 / 186. A regression that swaps the order (`token_budget` injected before `apply_token_budget_public` mutates `review`) produces inconsistent output. The empty-review short-circuit in `dispatch_review` (lines 144-150) returns a hardcoded JSON — if the schema diverges from `ReviewResult`'s serialized form, batch-mode callers see different field names than CLI-mode callers, silently breaking any pipeline that consumes both modes.
- **Suggested fix:** Pick the two highest-value handlers: `dispatch_review` and `dispatch_ci`. Add `test_dispatch_review_empty_diff_returns_hardcoded_skeleton` — construct a BatchContext, call with base=None and a stubbed git diff that produces empty review, assert the JSON keys match CLI mode (`changed_functions`, `affected_callers`, `affected_tests`, `risk_summary`). And `test_dispatch_review_applies_token_budget_when_provided` — construct a batch ctx with mock-populated call graph, call with tokens=Some(50), assert returned JSON has `token_budget: 50` and the review fields are truncated.

## [cli/batch/pipeline.rs::execute_pipeline — zero integration tests for the fan-out engine]
- **Difficulty:** medium
- **Location:** src/cli/batch/pipeline.rs:149 (the pipeline executor with RT-INJ-1 security fix)
- **Description:** `execute_pipeline` is the core of `cqs chat` and `batch` mode's `|` operator. The tests in this file only cover `extract_names`, `is_pipeable_command`, and `split_tokens_by_pipe` — zero tests exercise the actual pipeline execution path. Specifically untested: (a) the RT-INJ-1 security fix at line 263 that inserts `--` before the extracted name to prevent flag injection — a regression removing this line would allow names like `--help` to be parsed as flags; (b) the stage-0 execution → extract_names → stage-1 fan-out flow; (c) the PIPELINE_FAN_OUT_LIMIT truncation logic and `any_truncated` flag; (d) the empty-names short-circuit at line 250. The only test that would catch the `--` removal is a downstream break when `search foo | callers --help` crashes mid-pipeline.
- **Suggested fix:** Two minimal tests in pipeline.rs: (1) `test_execute_pipeline_fan_out_applies_double_dash_separator` — stage 0 returns a result with `name: "--help"`, stage 1 is `callers`, assert the downstream command receives `["callers", "--", "--help"]` and doesn't error on the flag-shaped name. Mock dispatch via a test-only `TestBatchContext` or by examining the dispatch tokens through a test-only hook. (2) `test_execute_pipeline_fan_out_truncates_at_limit` — seed stage 0 to return 2000 names, assert PIPELINE_FAN_OUT_LIMIT kicks in and `truncated: true` appears in the result.

## [resolve_splade_model_dir — env var tests exist, but vocab probe has no shape tests]
- **Difficulty:** easy
- **Location:** src/splade/mod.rs:85 (probe_model_vocab shape-branch selection)
- **Description:** `probe_model_vocab` selects between `sparse_vector` (2D) and `logits` (3D) ONNX output formats, validating shape dimensions. Both branches fail out on shape mismatch with specific error messages. Zero tests for either branch — the only tests that exercise this path are `#[ignore]`-gated SpladeEncoder::new tests that require an actual model. A regression that swaps `shape.len() != 2` to `shape.len() != 3` in the sparse_vector branch would silently accept 3D tensors from the wrong model architecture, producing garbage vocab sizes. Can't easily test without a real ONNX session, BUT the shape-dimension validation logic is testable via a pure helper.
- **Suggested fix:** Extract shape validation to a pure helper `validate_sparse_vector_shape(shape: &[u32]) -> Result<usize, SpladeError>` and `validate_logits_shape(shape: &[u32]) -> Result<usize, SpladeError>`. Add three tests each: valid 2D/3D shape returns vocab dim, 1D shape returns InferenceFailed error, 4D shape returns InferenceFailed error. Refactor `probe_model_vocab` to call the helpers. This also covers `encode` at mod.rs:430 and `encode_batch` at mod.rs:653 which have the same shape-checking logic duplicated.

## [cli/commands/io/drift.rs::build_drift_output — limit truncation untested]
- **Difficulty:** easy
- **Location:** src/cli/commands/io/drift.rs:40 (the limit-applying wrapper)
- **Description:** `build_drift_output` takes a `DriftResult` and an `Option<usize>` limit, truncating `drifted` entries to the limit while preserving `total_compared` and `unchanged`. The existing tests in drift.rs (`drift_output_empty`, `drift_output_serialization`) manually construct a `DriftOutput` and test serialization — they never call `build_drift_output` with a real `DriftResult`, so the limit path is untested. A regression that applies the limit to `total_compared` instead of `drifted`, or that forgets `Some(lim)` case entirely, would silently change drift output behavior.
- **Suggested fix:** Two tests in drift.rs test module: (1) `test_build_drift_output_respects_limit` — construct a DriftResult with 10 drifted entries, call `build_drift_output(&result, Some(3))`, assert `output.drifted.len() == 3` AND `output.total_compared == 10`. (2) `test_build_drift_output_no_limit_returns_all` — call with `None`, assert all 10 returned. (3) `test_build_drift_output_path_normalized` — regression test for the v1.15.1 finding that fixed `.display().to_string()` → `normalize_path` — verify a Windows-style path in the input is normalized on the way out.

## [cli/commands/infra/reference.rs::cmd_ref_add — zero tests for reference index build]
- **Difficulty:** medium
- **Location:** src/cli/commands/infra/reference.rs:87 (the heavy-lifting add-reference orchestrator) and 186 (cmd_ref_list), 266 (cmd_ref_remove), 303 (cmd_ref_update)
- **Description:** The reference subcommands (`cqs ref add`, `list`, `remove`, `update`) are the only way users register external reference indexes for `--ref` search. `cmd_ref_add` opens a store, indexes the source files, builds an HNSW, writes reference metadata. The only tests in reference.rs are `test_ref_list_entry_serialization` and `test_ref_list_entry_no_source` — both construct a `RefListEntry` struct and verify its JSON shape. None of the actual command handlers have tests. A regression that breaks the add-then-list round-trip is only caught when a user runs `cqs ref add` and then notices no output.
- **Suggested fix:** Two tests: (1) `test_cmd_ref_add_persists_metadata` — call `cmd_ref_add(&cli, "test", &source_path, 0.5)` with a tempdir source containing one .rs file, assert `refs_dir().join("test")` exists and `cmd_ref_list(&cli, true)` returns a single entry with name="test", chunks=1. (2) `test_cmd_ref_remove_cleans_up` — add then remove, assert `refs_dir().join("test")` is gone. These require a real Embedder so they should be `#[ignore]`-gated alongside the existing model-dependent tests, but they should exist as a tripwire against accidental refactors.

## [cli/commands/infra/project.rs::cmd_project — subcommand dispatch untested]
- **Difficulty:** easy
- **Location:** src/cli/commands/infra/project.rs:67 (the cross-project ProjectCommand dispatcher)
- **Description:** `cmd_project` handles `cqs project add`, `list`, `remove`, `search`. Only the `ProjectSearchResult` struct is tested (serialization). None of the actual subcommand dispatchers — including the cross-project search logic that reads the global project registry and iterates stores — has any test. A regression in the subcommand branch match (e.g. swapped `Add` and `Remove` branches) would silently reverse the operations.
- **Suggested fix:** `test_cmd_project_add_then_list_returns_added_project` — create a tempdir registry, call `cmd_project(&ProjectCommand::Add { path, name })`, then `cmd_project(&ProjectCommand::List { json: true })`, assert the output contains the added project name. Companion `test_cmd_project_remove_then_list_omits_removed` — same flow with remove + list. Mock the registry path via `CQS_PROJECT_REGISTRY` env var (if one exists) or a new test-only override.

---

