# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **SPLADE index persistence** — on-disk `splade.index.bin` with blake3 integrity, generation-counter invalidation, and lazy rebuild. Warm SPLADE query: 45s → 9.7s (#895).
- **v19 migration** — `sparse_vectors` gains FK `ON DELETE CASCADE` to `chunks`. Orphan sparse rows from chunk deletion are structurally impossible (#898).
- **v20 migration** — `AFTER DELETE` trigger on `chunks` auto-bumps `splade_generation`, closing the watch-mode SPLADE drift (#899).
- `CQS_SKIP_INTEGRITY_CHECK` env var — opt-out from `PRAGMA quick_check` on write opens.
- `CQS_SPLADE_MAX_INDEX_BYTES` env var — cap on persisted SPLADE index file size (default 2 GB).
- `CQS_EVAL_TIMEOUT_SECS` env var — per-query timeout for the eval harness (default 300s).

### Fixed
- **86s → 6.9s per search query** — `PRAGMA integrity_check(1)` on every `Store::open` was walking the full 1.1 GB database. Read-only opens now skip the check entirely; write opens use `quick_check` (#893).
- **Eval harness query-as-subcommand** — single-token queries like `prepare_for_embedding` were parsed as unknown subcommands. Fixed with `cqs --json -n 20 -- <query>` form (#894).
- **OpenRCT2 dual-trail spec revision** — removed time estimates, off-ramps, and substrate grading per session feedback (#896).

### Performance
- SPLADE persistence: 45s cold → 9.7s warm per SPLADE query (on-disk load instead of rebuild from SQLite) (#895).
- Sparse DELETE batch size: `chunks(333)` → derived from SQLite variable limit (SHL-31 fix in #898).
- SPLADE body `Vec::new()` → `Vec::with_capacity(body_len)` from file metadata (PF-4 fix in #898).

## [1.22.0] - 2026-04-09

### Added
- **Adaptive retrieval** — query classifier routes queries to optimal search strategy. Identifier queries skip embedding (NameOnly via FTS5), structural queries get type boost (1.2x). 28 classifier tests.
- **SPLADE pre-pooled output** — SpladeEncoder auto-detects pre-pooled (2D) vs raw logits (3D) ONNX output. Enables SPLADE-Code 0.6B (Naver) models.
- **Routing telemetry** — `log_routed()` tracks category, confidence, strategy, and fallback per search query.
- **Type boost in scoring** — `SearchFilter.type_boost_types` field applies 1.2x score multiplier for matching chunk types (boost, not filter).

## [1.21.0] - 2026-04-09

### Added
- **Cross-project call graph** — `--cross-project` flag on callers, callees, deps, impact, search, explain (#850).
- **4 new chunk types** — Extern, Namespace, Middleware, Modifier (29 total) (#851).

### Changed
- **README eval numbers** — live eval table updated to v2 265q results (48.5%/66.7% baseline, 48.5%/67.9% with summaries). Fixture eval unchanged (91.2% R@1).
- **Cargo.toml description** — updated to 91.2% R@1 / 0.951 MRR.

### Fixed
- **Chunk type coverage gaps** across 15 languages — constructor, test, extern, extension reclassification (#852).
- **OB-7**: SPLADE model-not-found log promoted from debug to warn.
- **OB-8/9**: Watch mode tracing for skipped events and pending_files overflow.
- **OB-10/12**: Missing tracing spans on `search_single_project` and `load_single_reference`.
- **DOC-36**: Doc comments on 8 undocumented post_process functions.
- **DOC-34**: `--include-types` corrected to `--type-impact` in README impact example.
- **DOC-35**: `cross_project.rs` added to store/calls module listing in CONTRIBUTING.md.

## [1.20.0] - 2026-04-08

14-category code audit: 71 findings, 69 fixed, 2 tracked issues (#843, #844).

### Added
- **Batch `--include-type`/`--exclude-type`** — agents can now filter by chunk type in batch mode, matching CLI parity (#842).
- **`CQS_SPLADE_THRESHOLD` env var** — SPLADE sparse activation threshold configurable (default 0.01) (#842).
- **`SpladeEncoder::default_threshold()`** — reads env var with fallback (#842).
- **`ChunkTypeMap` type alias** — public type for chunk type + language map (#842).
- **Environment Variables table in README** — 14 operational knobs documented (#842).
- **EXT-1 exhaustive test** — compile-time check that every ChunkType variant is classified in `is_code()` (#842).
- **8 P4 audit tests** — HnswIndex::search_filtered, cache boundary crossing, NaN embeddings, chunk type capture names, prune edge cases, duplicate hash behavior (#841).
- **Elm language support** — 54th language (#840).

### Fixed
- **AC-2**: Watch mode chunk ID rewrite — absolute → relative path in ID, preventing duplicate chunks (#842).
- **SEC-10**: Cache evict() negative size guard + logical data measurement (#841).
- **SEC-7/8/9**: Cache SQLite path encoding, DB permissions 0o700/0o600, query log 0o600 (#840, #841).
- **DS-45**: Model fingerprint fallback appends timestamp to prevent cache key collisions (#842).
- **DS-47/49/50**: Cache busy_timeout, logical eviction sizing, multi_thread runtime (#840, #841).
- **RB-10/11/12/13**: SPLADE poisoned mutex, format_timestamp panic, splade_alpha validation, input truncation (#840, #841).
- **PF-6/8/9/11/12/13/14**: Cache buffer reuse, hash dedup, fold optimization, batched DELETE, chunk_type_map caching, direct indexing, zero-copy logits (#840, #841, #842).
- **OB-1–6**: Stats/evict/read_batch/log_query/dispatch/cache_cmd observability (#841).
- **EH-13/14/15/16**: SPLADE batch, ensure_splade_index, neighbors, prune_orphan wired into GC (#841, #842).
- **CQ-2/4/5/6/7**: Dead code removal, parent context dedup, batch type filters, rerank guard (#841, #842).
- **AC-1/3**: Evict min deletions, bootstrap CI seed quality (#842).
- **PB-1/2**: SQLite pool sizing, idle timeout WAL retention (#842).
- **RM-2**: Cache pool connection count (#842).
- **DOC-27–31**: Language count, chunk type count, eval baselines, stale comments (#842).

### Changed
- **README eval section** — fixture (91.2%) and live (48.5%/69.3%) evals presented separately with context.
- **SPLADE code fine-tuning** — completed, null result (0.0pp R@1). Failure mode analyzed: weak regularization, BERT vocab, dense-mined negatives. v2-v4 experiments planned.

## [1.19.0] - 2026-04-07

### Added
- **JS/TS test block detection** — `describe()`, `it()`, `test()` call expressions captured as Test chunk type. Jest/Mocha/Vitest compatible (#836).
- **Python Flask endpoint detection** — `@app.route`, `@app.get`, `@app.post`, etc. detected as Endpoint chunk type via post_process (#836).

### Changed
- **Unified capture name lists** — three hardcoded chunk type arrays replaced with `ChunkType::CAPTURE_NAMES` generated by the `define_chunk_types!` macro. Adding a chunk type now requires one edit, not three (#836).
- **Store dim check on model switch** — pipeline skips store embedding cache when embedder dim != store dim, preventing silent reuse of wrong-model embeddings (#835).

## [1.18.0] - 2026-04-07

### Added
- **Global embedding cache** — transparent acceleration layer at `~/.cache/cqs/embeddings.db`. Caches ONNX embeddings keyed by `(content_hash, model_fingerprint)`. Pipeline checks cache before running inference. LRA eviction (no write-on-read). `cqs cache stats|clear|prune` CLI commands (#831).
- **Model fingerprint** — blake3 hash of the ONNX model file, computed lazily. Cache entries are tied to a specific model binary, not just its name (#831).
- **5 new chunk types** — Test (Rust `#[test]`, Python `test_`, Go `Test` prefix), Variable (`static mut`, `let`/`var`, module-level assignments), Endpoint (capture ready, framework queries Phase 2), Service (protobuf service definitions), StoredProc (SQL procedures, views, triggers). 27 total chunk types (#833).
- **V2 eval harness** — ablation matrix runner with bootstrap CIs (10k resamples), paired comparisons, per-category breakdown, per-query result storage (JSONL), markdown report generation. 112-query set across 8 categories (#832).
- **Batch query logging** — search, gather, scout, onboard, where, task queries logged to `~/.cache/cqs/query_log.jsonl` for eval workflow capture (#832).

### Changed
- **Zero `clippy::too_many_arguments` suppressions** — extracted `EmbedStageContext`, `RefQueryContext`, `SweepConfig` structs (#831).

## [1.17.0] - 2026-04-06

### Added
- **SPLADE sparse-dense hybrid search** — opt-in via `--splade` flag (default off, zero overhead when disabled). Learned sparse retrieval alongside dense cosine using off-the-shelf `naver/splade-cocondenser-ensembledistil` (110M). `--splade-alpha` tunes fusion weight (0=pure sparse rerank, 1=pure cosine). +2pp R@1 function lookup, +5pp conceptual queries on real code. Schema v17 adds `sparse_vectors` table (#829).
- **HNSW traversal-time filtering** — `--chunk-type` and `--lang` filters applied during HNSW graph walk instead of post-filter. Returns exactly k matching results (#826).
- **ConfigKey chunk type** — JSON/TOML/YAML/INI keys were Property (callable), polluting code search. New ConfigKey type excluded from code search by default (#818).
- **Impl chunk type** — Haskell `instance` declarations correctly typed as Impl, not Object (#819).
- **`enrichment_version` column** — RT-DATA-2 idempotency marker for enrichment passes (schema v17).
- **Config G** in fixture eval — SPLADE rerank configuration.

### Fixed
- **RRF disabled in batch mode** — was 17pp worse than cosine-only. Cosine-only is now the default for all search paths (#827).
- **LLM summary preservation** — `--force` reindex reads summaries into memory before DB rename, restores after. No more redundant API calls (#820).
- **CAGRA itopk_size cap** — clamped to 512 (cuVS limit). Was silently returning empty results when `search_with_filter` over-fetched (#829).
- **prune_missing path mismatch** — suffix matching catches absolute/relative ID mismatches from incremental reindex (#829).
- **VB.NET @constant → @const** capture fix (#829).
- **Code-only filter in batch search** — batch mode was missing `ChunkType::code_types()` filter (#818).

### Changed
- **Eval renames** — `test_pipeline_scoring` → `test_fixture_eval_296q`, `test_stress_eval` → `test_noise_eval_143q`, `run_hard_eval.py` → `run_raw_eval.py` (#818).
- **Schema v17** — `sparse_vectors` table + `enrichment_version` column.

## [1.16.0] - 2026-04-05

### Added
- **Dart language support** — 53rd language. Functions, classes, enums, mixins, extensions, methods, getters/setters, doc comments (#816).

### Changed
- **Language macro v2** — consolidated 52 per-language `.rs` files into `src/language/languages.rs` with `..DEFAULTS` spread + 106 `.scm` query files in `src/language/queries/`. Adding a language is now: write `.scm` queries, add one `LanguageDef` static, register in `define_languages!` (#815).
- **331 language tests** moved to `tests/language_test.rs` (integration tests). 31 private-function tests dropped.
- **CONTRIBUTING.md** — "Adding a New Language" guide rewritten for the consolidated system. Architecture overview updated.
- **ROADMAP.md** — cleaned from 528 to 111 lines. Done items collapsed into summary table.

## [1.15.2] - 2026-04-04

10th code audit: 103 findings, 103 fixed. Zero P1/P2/P3/P4 remaining. Typed JSON output structs across all commands. 35 PRs (#776-812).

### Added
- **Typed JSON output structs** — `#[derive(Serialize)]` structs replace all manual `json!({})` builders. Consistent field naming (`line_start`/`line_end`, `name`, `score`, `file`) across all `--json` output (#776-783, #788, #797).
- **JSON naming conventions** documented in CONTRIBUTING.md (EXT-42, #807).
- **`CommandContext::open_readwrite()`** — writable store access for batch mode (AD-17, #798).
- **53 new tests** — batch integration (HP-4, #802), SearchResult/CommandContext unit tests (HP-7/HP-8, #803), adversarial parser tests (TC-8/9/10, #796), edge case coverage (TC-7/11-16, HP-1-9, #793/#807/#809).

### Fixed
- **10 P1 findings** — FTS injection guard compiled out in release (SEC-10), unicode panic in telemetry (RB-7), BFS node cap checked before target (AC-7), RRF K env var read per-search (PF-2), wrong doc_format match arm (EXT-39), field name normalization across 3 output types (AD-11/13/15), token_pack zero-budget guarantee (TC-6), is_hnsw_dirty wrong default (EH-7) (#785-788, #793).
- **13 P2 findings** — 3 divergent search JSON constructors unified (CQ-NEW-3/5/7, #797), batch context schema mismatch (AD-14, #798), onboard token packing deduplicated (CQ-NEW-4, #801), BFS unbounded memory (AC-10, #794), deferred vecs unbounded during indexing (RM-9, #800), HNSW lock released too early (DS-NEW-1, #799), impact_to_json double-pass serialize (CQ-NEW-6/PF-3, #794).
- **33 P3 findings** — N+1 DB queries in map_hunks_to_functions (PF-1, #805), blake3 recomputation (PF-4, #805), hardcoded limits now env-var configurable (SHL-17/18/19, #795), open_readonly actually readonly (RM-7, #806), notes double store connection (RM-8, #806), stale 768-dim doc comments (SHL-21, #810), dead SuggestOutput code (AD-18, #810), model_config panic before resolve (RB-8, #810), notes cache RwLock→Mutex race fix (DS-NEW-4, #811), 6 silent error fallbacks now warn (EH-8/9/10/11/12, #804), telemetry unbounded growth (SHL-20, #786), L5K regex backtracking (SEC-8, #796), path normalization in diff/explain output (PB-8/9, #789), window overlap edge case (AC-8, #806), env var test race (DS-NEW-3, #810).
- **47 P4 findings** — 9 tracing spans (OB-1-9, #790), 10 stale doc paths/counts (DOC-11-20, #791), telemetry memory reduction (RM-10/11, #808), all adversarial and happy-path test gaps (#793/#796/#807/#809).

### Changed
- **Docs review** — README R@1 94.5% → 90.9% (current 296-query eval). CONTRIBUTING.md architecture listing updated with 4 missing source files (#812).

## [1.15.1] - 2026-04-04

### Changed
- **CI rust-cache** — clippy 2m→41s, msrv 2m→43s (#764)
- **cli/mod.rs split** — store openers, CommandContext, vector index builder extracted to `cli/store.rs` (#765)
- **pipeline.rs split** — 1303 lines into 6 submodules: types, windowing, embedding, parsing, upsert (#766)
- **store/helpers.rs split** — 1222 lines into 7 submodules: error, rows, types, search_filter, scoring, sql, embeddings (#768)
- **Telemetry subcommands** — derive from clap `CommandFactory`, replaces 44-entry hardcoded list (#767)
- **Lazy reranker** — `OnceLock` in `CommandContext`, eliminates per-search ONNX session creation (#769)
- **Lazy embedder** — `OnceLock` in `CommandContext`, eliminates 8 per-command `Embedder::new()` calls (#774)
- **Batch/CLI unification** — 3 phases: shared functions for deps/related/test_map (#771), JSON builders for trace/stats/stale/where (#772), token-packing utilities for scout/onboard/gather/search (#773)
- **Eval script** — fixed schema mismatch for expanded eval format, corrected CoIR averaging method (#763)

### Fixed
- **CI test race** — env var race condition in embed batch size tests (#770)

## [1.15.0] - 2026-04-02

### Added
- **L5X parser** — extract Structured Text from Rockwell/Allen-Bradley Logix Designer XML exports. Regex CDATA extraction → ST tree-sitter grammar. Programs, routines, AOIs (#755).
- **L5K parser** — legacy ASCII format support alongside L5X. Keyword-delimited blocks (`ROUTINE...END_ROUTINE`), `ST_CONTENT := [...]` and `N:0` numbered line formats (#758).
- **`cqs telemetry`** — usage dashboard with command frequency, category breakdown (Search/Structural/Orchestrator/Read-Write/Infra), session detection, top queries. `--reset` for archival, `--all` for history, `--json` output (#754).
- **6 custom agent definitions** in `.claude/agents/` — investigator, code-reviewer, test-finder, implementer, explorer, auditor. Agents have cqs commands built in (#753).
- **CLAUDE.md stability guidance** — "Remain calm. There is no rush." preamble and "When Stuck" section (3-strike rule, agent dispatch, diagnose-first) based on Anthropic emotion concepts research (#756).

### Changed
- **CommandContext refactor** — shared `CommandContext` struct replaces 32 independent `open_project_store_readonly()` calls. Constructed once in dispatch, passed to all store-using handlers (#757).
- **Commands subdirectory restructure** — 45 command files organized into 7 thematic subdirectories: search/, graph/, review/, index/, io/, infra/, train/. File history preserved via `git mv` (#760).
- **CLAUDE.md tightened** — 379 → 273 lines. Agent instructions moved to `.claude/agents/` (#753).

### Removed
- **CI eval job** — was tag-only, CPU-only (different path than shipped GPU), redundant with local evals (#754).

### Fixed
- **Docs audit** — README R@1 94.5% → 90.9% (expanded eval), added telemetry command, L5X/L5K refs. CONTRIBUTING.md architecture updated. SECURITY.md/PRIVACY.md telemetry mentions added. GitHub repo description updated (#759).

## [1.14.0] - 2026-04-02

### Added
- **187-query real codebase eval** — 100 function lookup, 40 conceptual, 20 callgraph, 27 gitblame queries against cqs itself. v9-200k 49% R@1, BGE-large 48%, nomic 32%.
- **`--format text|json`** on all 26 remaining commands via `TextJsonArgs` migration (#751).
- **`[scoring]` config section** — runtime override of all 9 scoring parameters via `.cqs.toml` (#750).
- **`ImpactOptions`** struct replaces 5 positional params in `analyze_impact` (#749).
- **`GatherContext`**, **`QueryContext`**, **`ScoutResources`** option structs — eliminates all `clippy::too_many_arguments` (#747).
- **Shared cmd/batch JSON builders** — `callers_to_json`, `callees_to_json`, `dead_to_json`, `stale_to_json`, `similar_to_json` (#747).
- **`load_eval_cases_from_json()`** — load eval queries from external JSON files (#750).
- **`StoreError::EmbeddingBlobMismatch`** — typed errors for dimension mismatches (#750).
- **`CQS_CAGRA_MAX_BYTES`** env var — configurable GPU staging memory limit (#746).
- **`CQS_RRF_K`**, **`CQS_WATCH_*`**, **`CQS_MD_*`** env var overrides (#744).
- **`CQS_QUERY_CACHE_SIZE`** env var for batch/REPL cache tuning (#744).
- **Per-query eval diagnostics** (`CQS_EVAL_OUTPUT`) + enrichment ablation (`CQS_SKIP_ENRICHMENT`) (#740).

### Fixed
- **~40 audit findings** resolved across P1-P4 (PRs #737-751). Zero P1/P2/P3 remaining.
- **SEC-4**: Canonicalize convert `--output` directory, warn if outside source tree.
- **RB-5/AC-1/DS-43/CQ-2/PB-7**: P2 fixes — cross-project score normalization, batch dim warning, shared test_map, .cqs-lock for doc writer.
- **EH-1/5/6**: Error handling — `to_value().ok()` replaced with tracing, `set_permissions` failures logged, notes parse error visible to user.
- **TC-1/4/5**: Added NaN/Inf tests for reranker and score_candidate, placement test.

### Changed
- **Code-only search default** — `--include-docs` flag for documentation chunks (#741).
- **`gather()`** takes `&Embedder` instead of pre-computed embedding (#751).
- **lib.rs re-exports** — wildcard `pub use module::*` replaces ~70 itemized imports (#747).
- **`DeadArgs`**, **`StaleArgs`** shared between CLI and batch via `#[command(flatten)]` (#747).
- **Windowing overlap** scales with model context: `max(64, window/8)` (#743).
- **Reranker config** wired into `.cqs.toml` (`reranker_model`, `reranker_max_length`) (#743).
- 5,445 lines of boilerplate doc comments stripped (#739).

### Removed
- 5 `clippy::too_many_arguments` suppressions (replaced with option structs).
- Duplicate `find_python` (shared in `convert::find_python`).
- Dead `display_similar_results_json`, `display_dead_json`.

## [1.13.0] - 2026-03-31

### Added
- **`cqs task --brief`** — compact ~200 token output: files to touch, placement suggestions, at-risk functions, test coverage. Both text and JSON output. Designed for agent context windows.
- **v9-200k LoRA preset** — `CQS_EMBEDDING_MODEL=v9-200k` or config `model = "v9-200k"`. 110M model achieving 90.5% R@1 on expanded eval, virtually ties BGE-large (90.9%) at 1/3 the size.
- **Stop hook** — `cqs review` runs on session end, surfaces diff risk via systemMessage.
- **Expanded pipeline eval** — 296 queries across 7 languages (added Java + PHP). Replaces 55-query eval.

### Fixed
- **FTS5 synonym expansion** — OR groups require explicit AND between terms (`(a OR b) AND c`, not `(a OR b) c`). Crashed on queries with expanded tokens like "parse command-line".
- **Pipeline eval resilience** — search errors treated as misses instead of panicking.
- Removed unused test imports (`crud.rs` Chunk, `staleness.rs` PathBuf).

### Changed
- **CLAUDE.md** — added 5 missing skills, 8 missing commands to reference lists.
- **ROADMAP.md** — marked 8 completed items, collapsed stale v1.1.0 release plan, updated eval tables.
- **Bootstrap skill** — added 5 missing portable skills, 15 missing commands to template.

### Dependencies
- `proptest` 1.10.0 → 1.11.0
- `tree-sitter-rust` 0.24.1 → 0.24.2
- `toml` 1.0.7 → 1.1.0
- `insta` 1.46.3 → 1.47.1
- `rustyline` 17.0.2 → 18.0.0

## [1.12.0] - 2026-03-30

### Fixed
- **#711** RT-RES-9: Diff impact analysis capped at 500 changed functions (was unbounded).
- **#695** EX-32: `export-model` auto-detects embedding dimension from `config.json` `hidden_size`.
- **#694** EX-30: `BatchProvider::is_valid_batch_id` moved to trait method (was hardcoded Anthropic `msgbatch_` prefix).
- **#697** SEC-22: `cargo audit` config ignores 3 transitive unmaintained advisories (bincode, number_prefix, paste).
- **#716** PERF-45: `EMBED_BATCH_SIZE` restored to 64 (was 32 after undiagnosed crash). Debug logging added. Full reindex verified stable.

### Changed
- **#718** CQ-38: `parser/markdown.rs` (2030 lines) split into `markdown/` directory with 4 files (`mod.rs`, `headings.rs`, `code_blocks.rs`, `tables.rs`). All 3 `clippy::too_many_arguments` suppressions resolved with context structs.
- **CONTRIBUTING.md**: Added "Adding a New CLI Command" checklist (10-item process).
- **v9-200k model published** to HuggingFace (`jamie8johnson/e5-base-v2-code-search`).

## [1.11.0] - 2026-03-29

### Added
- **`cqs brief <file>`** — one-line-per-function summary with caller count and test coverage.
- **`cqs affected`** — diff → changed functions → callers → tests → risk score. The "before you commit" command.
- **`cqs neighbors <fn>`** — embedding-space nearest neighbors with cosine similarity scores.
- **`cqs doctor --fix`** — auto-remediate diagnosed issues (stale index, schema mismatch).
- **`cqs train-pairs`** — extract training pairs from index as JSONL, with `--contrastive` for call-graph-based "Unlike X" prefixes.
- **Query expansion** — static synonym map (31 programming abbreviations). "auth" also matches "authentication", "authorize", "credential". Zero-cost OR-based FTS expansion.
- **`VectorIndex::dim()` trait method** — both HnswIndex and CagraIndex expose dimension through the trait.
- **`CQS_CAGRA_THRESHOLD` env var** — override the 5000-vector threshold for GPU-accelerated CAGRA index.
- **`CQS_HYDE_MAX_TOKENS` env var** — configure HyDE prediction token budget (default: 150).
- **`OutputArgs`/`TextJsonArgs` shared structs** — reduced per-command boilerplate for `--json`/`--format` flags.
- **`WatchConfig`/`WatchState` structs** — replaced 12-parameter `process_file_changes` with clean context structs.
- **Pre-Edit hook** (`.claude/hooks/pre-edit-context.py`) — auto-injects module context for `.rs` files.
- **43 new tests** (1540 total).

### Changed
- **`ModelInfo.dimensions`: u32 → usize** — eliminates `as u32`/`as usize` casts.
- **`ModelInfo` moved to `embedder::models`** — re-exported from `store::helpers`.
- **`full_cosine_similarity` uses f64 accumulators** — precision fix at 1024+ dimensions.
- **Contrastive neighbor extraction: O(N) via `select_nth_unstable_by`** — was O(N²).
- **Enrichment batch: single UPDATE...FROM join** — was N individual UPDATEs per transaction.
- **Notes cache: `Arc<Vec<NoteSummary>>`** — was deep-clone per search.

### Fixed
- 80 of 88 audit findings fixed across 14 categories. Key fixes:
  - `build_with_dim(dim=0)` panic → returns Err
  - Empty chunk names → zero-vector embeddings → file path fallback
  - `delete_phantom_chunks` SQLite 999-parameter limit → temp table
  - Batch response 100MB cap bypassed by chunked encoding → `Read::take()`
  - BFS inner-loop node cap prevents 10K→15K overshoot
  - `grammar()` → `try_grammar()` at all 9 parser call sites
  - `clamp_config_f32` NaN passthrough → clamped to min
  - 9 stale E5-base-v2/768-dim references → BGE-large/1024
  - See `docs/audit-triage.md` for the full list.

## [1.9.0] - 2026-03-27

### Changed
- **Default model: BGE-large-en-v1.5** — 94.5% pipeline R@1 vs 83.6% for E5-base (+10.9pp). Larger download (~1.3GB vs ~547MB) but significantly better search quality. E5-base remains available as a preset via `CQS_EMBEDDING_MODEL=e5-base`.
- **`EMBEDDING_DIM`: 768 → 1024** — matches BGE-large. Tests and helpers are now dim-agnostic.
- **`ModelConfig::default_model()`** — single source of truth for the default. `DEFAULT_MODEL_REPO`, `DEFAULT_DIM`, `EMBEDDING_DIM`, `ModelInfo::default()`, and serde defaults all derive from it. Changing the default model is now a one-line change.

### Added
- **`CQS_ONNX_DIR` env var** — load local ONNX models without HuggingFace download. Point at a directory with `model.onnx` + `tokenizer.json`.
- **`DEFAULT_DIM` constant** — exported from embedder, used by `EMBEDDING_DIM`.
- **Consistency test** — verifies `DEFAULT_MODEL_REPO` and `DEFAULT_DIM` match `default_model()` at test time.

### Fixed
- **`Store::set_dim()`** — syncs in-memory dim after `init()` for non-default models.
- **`init()` uses resolved model** — was using `ModelInfo::default()`, causing dim mismatch for BGE-large.
- **All HNSW convenience wrappers deleted** — `build()`, `build_batched()`, `load()`, `try_load()` removed. Only `_with_dim` variants remain.
- **Metric correction** — historical "92.7% R@1" was Relaxed R@1 (top-2). Strict R@1: 94.5% (BGE-large), 83.6% (E5-base).

## [1.8.0] - 2026-03-27

### Fixed
- **Multi-model support functional end-to-end** — `--model` flag was parsed but ignored at all 20 call sites; Store rejected non-default models on open; HNSW hardcoded 768-dim. All fixed: ModelConfig resolved once in dispatch, threaded through all commands, HNSW uses `store.dim()`, Store skips model validation on open.
- **dim=0 validation** — zero dimension accepted silently through ModelConfig → Store → HNSW, producing empty search results. Now rejected at all three layers with warnings.
- **Batch data safety** — `resume()` returned unfiltered results (inflated counts); hash validation failure stored all results including stale data. Fixed: returns valid results only, hash failure skips storage.
- **85 audit findings** — 7th full audit (v1.7.0), 85 of 95 findings fixed across 14 categories. 5 issues created, 2 closed as wontfix.

### Changed
- **`nl.rs` split** — 2051-line monolith split into `nl/{mod,fts,fields,markdown}.rs` (4 files by responsibility).
- **`BatchSubmitItem` struct** — replaces opaque `(String, String, String, String)` tuple across all batch submission APIs.
- **`Store::dim`** — changed from `pub` field to `pub(crate)` with `pub fn dim()` getter. Prevents accidental mutation.
- **`DEFAULT_MODEL_REPO`** — single source of truth in `embedder/models.rs`, referenced by store and helpers.
- **`should_skip_line`** — data-driven via `skip_line_prefixes` on all 51 `LanguageDef` definitions (was 12 hardcoded keywords).
- **`upsert_type_edges`** — extracted shared SQL logic, eliminating 120-line duplication between single-file and batch variants.
- **`create_client()` factory** — replaces 3 entry points each hardcoding `ANTHROPIC_API_KEY`.
- **`LlmProvider` enum** — extensible provider selection via `CQS_LLM_PROVIDER` env var (default: Anthropic).
- **`export-model`** — auto-detects dim from `config.json`, validates repo format (SEC-18), finds Python cross-platform (PB-29), canonicalizes paths (PB-30), sets file permissions (SEC-19).

### Added
- **24 new tests** — multi-model dim threading (TC-31), batch orchestration with MockBatchProvider (TC-32), `Embedding::try_new` edge cases, config NaN/dim=0, HNSW dim=0. Total: 1490.
- **`MockBatchProvider`** — test double for batch orchestration without API credentials (TC-38).
- **Wiring verification** — added to CLAUDE.md Completion Checklist as lesson from configurable models bug.

## [1.7.0] - 2026-03-26

### Added
- **Configurable embedding models** — `CQS_EMBEDDING_MODEL` env var, `--model` CLI flag, or `[embedding]` config section. Ships with E5-base-v2 (default) and BGE-large-en-v1.5. Custom ONNX models supported via `cqs export-model`.
- **`cqs export-model`** command — exports HuggingFace models to ONNX format for use with cqs.
- **`cqs doctor`** model consistency check — warns when configured model doesn't match indexed model.
- **Workflow skills** — `/before-edit`, `/investigate`, `/check-my-work` for agent-friendly cqs usage.
- **Task-triggered CLAUDE.md** — restructured from 35-command reference to task-triggered format with ownership framing.

### Changed
- **Runtime embedding dimension** — HNSW, CAGRA, and Store layers accept dynamic dimensions instead of hardcoded 768. Enables non-E5 models with different embedding sizes.
- **`Embedding::try_new()`** — now accepts any dimension > 0 (was hardcoded to 768).
- **`ModelConfig` resolution** — CLI flag > env var > config file > default. Backward compatible with existing `CQS_EMBEDDING_MODEL` usage.

## [1.6.0] - 2026-03-26

### Added
- **FieldStyle enum** — data-driven field extraction for 28 languages via `LanguageDef.field_style`. TypeFirst (C/C++/Java/C#/CUDA/GLSL) correctly extracts field names instead of types. NameFirst covers Rust/Go/Python/TS/JS/Kotlin/Swift/Scala/PHP/Ruby and 12 more. (#680)
- **BatchProvider trait** — LLM batch lifecycle abstracted behind trait. Anthropic implementation unchanged. Enables future provider additions. (#681)
- **Runtime embedding dimension** — `Embedder::embedding_dim()` detects dimension from ONNX model output via OnceLock. No longer hardcoded to 768. (#682)
- **Method keywords** — generic method extraction now recognizes `fun` (Kotlin), `sub` (Perl/VB), `proc` (Nim/Tcl), `method` (Raku).
- **Enrichment tests** — 12 new tests for NL caller/callee inclusion, IDF filtering, hash determinism.
- **`doc_convention` field** — language-specific doc comment conventions on LanguageDef for 31 languages.

### Changed
- **CLI split** — `cli/mod.rs` (2043 lines) split into `definitions.rs` + `dispatch.rs` + `mod.rs`.
- **Store split** — `store/mod.rs` (1573 lines) split into `metadata.rs` + `search.rs` + `mod.rs`.
- **CallGraph Arc\<str\>** — string interning halves memory for large call graphs (~40MB savings at 500K edges).
- **Lazy enrichment** — caller/callee maps loaded per page (~500 chunks) instead of pre-loading everything (~105MB). (#665)
- **GC single transaction** — all prune operations run atomically via `Store::prune_all()`. (#666)
- **Mtime millisecond precision** — staleness checks use milliseconds instead of seconds, preventing sub-second write misses on WSL/NTFS.
- **Contrastive neighbors** — N×N matrix capped at 15K chunks (OOM guard), dimension mismatch filtered, per-row sort O(n² log n) → O(n² log k) via BinaryHeap.
- **Batch size docs** — central documentation in `lib.rs` explaining the `floor(999/params)` formula. (#683)

### Fixed
- **82-finding audit** (6th full audit) — all P1-P4 findings fixed across 14 categories.
- **Security**: git subprocess argument injection (SHA + path validation), HTTPS-only API base warning.
- **Error handling**: LLM batch store errors no longer swallowed, stale batch results validated against current index, concurrent batch submission locked.
- **Robustness**: contrastive neighbor dimension panic, rayon pool double-unwrap, FTS whitespace-only query.
- **Platform**: cross-platform path separators (6 fixes), macOS case-insensitive prune guard.
- **Observability**: 4 missing tracing spans added, 7 silent degradation paths now warn.
- **API design**: `full_cosine_similarity` returns `Option<f32>`, `Client` → `LlmClient`, blame `-n` → `-d`, `CQS_LLM_API_BASE` primary env var, residual `to_json()` removed, `ScoringContext` struct.
- **Documentation**: stale LoRA references updated to base E5, download size corrected, metrics updated.
- **~1000 lines of noisy LLM-generated doc comments removed** from trivial functions. (#684)

## [1.5.0] - 2026-03-25

### Changed
- **Default model switched to base E5** (`intfloat/e5-base-v2`) — the inference-time enrichment stack (contrastive summaries, HyDE, call graph context, hybrid retrieval) dominates retrieval quality. Base E5 achieves 92.7% R@1 on hard eval and 96.3% through the full pipeline, matching or exceeding all LoRA variants. Users should run `cqs index --force` after upgrading to rebuild embeddings.
- **ONNX model path** — now loads from `onnx/model.onnx` subdirectory (matching `intfloat/e5-base-v2` HuggingFace repo layout).

### Added
- **Enriched hard eval** — `test_hard_with_summaries` injects pre-generated contrastive summaries into fixture embeddings. 92.7% R@1, 100% R@5.
- **KeyDAC augmentation script** — `augment_keydac.py` generates keyword-preserving query rewrites for training data.
- **Full model matrix eval** — `scripts/full_model_matrix_eval.sh` runs all models on A6000 with 3x median.
- **v9 synthetic training plan** — balanced oversampling + synthetic queries + curriculum scheduling.
- **CI Node.js 24** — `FORCE_JAVASCRIPT_ACTIONS_TO_NODE24=true`, `actions/checkout@v5`.

### Removed
- **Research log** — moved to private `cqs-training` repository.

## [1.4.2] - 2026-03-24

### Added
- **Contrastive LLM summaries** — precompute top-3 embedding neighbors per chunk, pass to LLM prompt. Produces summaries like "unlike heap_sort, this uses divide-and-conquer merging." Doc-comment shortcut removed — all callable chunks go through contrastive API.
- **34 adversarial tests** — parser malformed input (10), store concurrent access (4), search NaN/Inf/zero embeddings (10), contrastive neighbor edge cases (5), JSONL parsing edge cases (5).
- **`build_contrastive_prompt()`** — separate prompt function for neighbor-aware summaries.
- **`submit_batch_prebuilt()`** — identity prompt builder for pre-built prompts.

### Fixed
- **FTS path filter** — `--path` glob now applies to FTS keyword results in RRF fusion. Previously only the semantic path was filtered, causing unscoped results to contaminate rankings.
- **Full-pipeline eval** — scoped to fixture files, reports R@1/R@5/R@10/NDCG@10, accepts case variants for Go `ValidateURL`/`ValidateUrl`.

### Changed
- **Audit skill** — Test Coverage category now checks for adversarial/edge-case gaps. Mandatory first steps added for all 14 categories.
- **Full-pipeline metrics** — 92.7% R@1, 96.3% R@5, 0.9478 NDCG@10 (55 queries, 5 languages).

## [1.4.1] - 2026-03-24

### Fixed
- **Corrupt dimension metadata** — `check_model_version` now returns `StoreError::Corruption` instead of silently passing on unparseable dimension metadata, preventing garbage search results.
- **Windows path separator** — `prune_missing` and `list_stale_files` now use `origin_to_pathbuf()` for DB-to-native path conversion, fixing total index deletion on native Windows.
- **UTF-8 panic** — `cmd_query` query preview uses `floor_char_boundary(200)` instead of byte slice, preventing panic on CJK/emoji queries.
- **Watch dirty flag** — clears `hnsw_dirty` on reindex failure (HNSW still matches pre-failure state), preventing permanent brute-force fallback.
- **API key exfiltration** — warns when `CQS_API_BASE` is non-default or non-HTTPS, since API key is sent to that URL.
- **Wrong-file snippet** — `extract_call_snippet_from_cache` now prefers same-file chunks for common names like `new()`.
- **Callable type filter** — `related.rs` uses `is_callable()` instead of hardcoded `Function|Method`, including Constructor/Property/Macro/Extension.
- **Reranker panic** — guards `outputs[0]` with error instead of index panic on corrupt ONNX model.
- **DiffImpactResult degraded** — added `degraded: bool` field to signal when callers/snippets were lost due to store errors.
- **Pipeline error counting** — `PipelineStats` now tracks `call_write_errors` and `type_edge_write_errors`.
- **BFS test counting** — `compute_risk_and_tests` uses `test_reachability` (forward BFS) matching `compute_risk_batch`, preventing divergent risk scores between commands.
- **HNSW rollback safety** — save now backs up old files before overwriting; rollback restores them instead of deleting.
- **Token budget overshoot** — waterfall budgeting subtracts actual usage from remaining, making budget a hard cap.
- **Batch ID validation** — API-returned batch IDs validated before storage.
- **Absolute path bypass** — `read_context_lines` now rejects absolute paths alongside `..` traversal.
- **Type impact degraded** — `find_type_impacted` errors now set the `degraded` flag on `ImpactResult`.
- **BM25 corpus errors** — parse failures during corpus build now counted and logged.
- **Predictable temp files** — `doc_writer/rewriter.rs` uses `temp_suffix()` with cleanup on write failure.
- **Windows executables** — `find_python` adds `py` launcher, `find_7z` adds default Windows path.
- **Language filter case** — SQL `IN` clause uses `COLLATE NOCASE` matching app-code behavior.
- **Checked hunk overflow** — `map_hunks_to_functions` uses `checked_add` instead of `saturating_add`.
- **WSL UNC paths** — poll-mode detection includes `//wsl.localhost/` and `//wsl$/` paths.
- **Stale docs** — ROADMAP version, MOONSHOT dimensions/notes behavior, plan.rs file path, doc comment references, notes.toml dimension, duplicated doc comment.

### Changed
- **JSON serialization** — paths are relative at construction time; `_to_json()` functions simplified to `serde_json::to_value()` wrappers. Consistent path format regardless of serialization path.
- **LLM batch dedup** — extracted `submit_batch_inner`, `BatchPhase2` struct, generic metadata methods. -215 net lines across `batch.rs`, `summary.rs`, `hyde.rs`, `doc_comments.rs`, `store/mod.rs`.
- **GateLevel/GateThreshold** — consolidated duplicate enums; `GateThreshold` now derives `clap::ValueEnum`.
- **IndexArgs struct** — `cmd_index` takes `IndexArgs` instead of 10 positional parameters, eliminating 14-line `#[cfg]` scaffolding.
- **Error type migration** — `gather()`/`find_related()` return `AnalysisError` instead of `StoreError`.
- **NL ChunkType classification** — uses `is_container()`/`has_parent_context()` methods instead of hardcoded matches.
- **Test name generation** — moved to `LanguageDef.test_name_suggestion` (65-line match → 4-line lookup).
- **Template fallback** — `plan.rs` finds "Fix a Bug" by name instead of positional index.
- **extract_patterns** — data-driven `LanguagePatternDef` replaces 383-line match (→109 lines).
- **test_reachability** — first-hop equivalence class optimization; reuses allocations.
- **Scout BFS batching** — single `compute_hints_batch` forward BFS replaces N per-function `reverse_bfs`.
- **Diff BFS attribution** — `reverse_bfs_multi_attributed` replaces N+1 separate BFS calls.
- **Batch query** — `map_hunks_to_functions` uses `get_chunks_by_origins_batch` (1 query vs N).
- **Batch transaction** — `upsert_function_calls_batch` wraps all files in single transaction.
- **Tensor copy** — `embed_batch` consumes ONNX output directly instead of `.to_vec()` (~50MB savings).
- **Reranker batching** — caps inference at 64 pairs per call to bound GPU memory.
- **BatchContext LRU** — reduced from 4 to 2 cached reference indexes (~400MB savings).
- **Watch HNSW eviction** — HNSW index freed on idle alongside embedder session.
- **Cross-project cap** — `search_across_projects` limited to 4 concurrent Store+HNSW opens.
- **Clone derives** — added to `PlanResult`, `GatherOptions`, `GatherResult`.
- **README** — collapsible `<details>` sections for Supported Languages and GPU Acceleration.

### Added
- **4 tracing spans** — `llm/batch.rs` functions, `upsert_chunks_and_calls`, 5 batch store methods, `compute_hints_with_graph`.
- **29 new tests** — `check_model_version` (4), `check_schema_version` (4), `resolve_target` (4), `compute_risk_batch` (6), JSONL parsing (9), chunk filtering (6), `reverse_bfs_multi_attributed` (7), `read_context_lines` absolute path (1).
- **`origin_to_pathbuf()`** — DB origin to native PathBuf conversion for Windows compatibility.
- **`is_container()`/`has_parent_context()`** — ChunkType classification methods for NL generation.
- **`compute_hints_batch()`** — batch forward BFS for scout/explain hint computation.
- **`upsert_function_calls_batch()`** — single-transaction multi-file function call insertion.

## [1.4.0] - 2026-03-24

### Added
- **Extension ChunkType** — Swift extensions, Objective-C categories, F# type extensions, Scala 3 extension definitions. Parser infrastructure wired (`capture_types`, `DEF_CAPTURES`).
- **Constructor ChunkType** — Detects constructors across 10 languages: Python `__init__`, Java/C#/Razor `constructor_declaration`, Kotlin `init`, Swift `init`, VB.NET `Sub New`, Rust `new`, Go `New*`, C++ (no return type), PHP `__construct`. NL: "constructor for {parent}".
- **Python/JS/TS constant capture** — UPPER_CASE module-level assignments as `ChunkType::Constant` with `post_process` filtering.
- **`--json` flag on impact/review/ci/trace** — alias for `--format json`. Backward-compatible with existing `--format` flag.
- **Batch/chat cache auto-invalidation** — mutable caches (HNSW, call graph, file set, notes) invalidate on `index.db` mtime change. `refresh`/`invalidate` command added.
- **`tests/full_pipeline_eval.sh`** — full-pipeline hard eval script (55 queries against live cqs index).

### Changed
- **Read-only commands use single-thread runtime** — `Store::open_light()` with `current_thread` tokio runtime for 27 read-only commands. Full 256MB mmap/16MB cache preserved.
- **Solidity events use `Event` ChunkType** instead of `Property`.
- **Java `static final` fields promoted to `Constant`** instead of `Property`.
- **Erlang `-define()` macros captured as `Macro`**.
- **Bash `readonly` declarations captured as `Constant`**.
- **R parser improved** — S4 classes (`setClass`), R6 classes (`R6Class`), UPPER_CASE constants.
- **Lua parser improved** — UPPER_CASE constants (local and global).

### Refactored
- **`llm.rs` (2245 lines)** split into 6 submodules: client, prompts, batch, summary, doc_comments, hyde.
- **`store/calls.rs` (1570 lines)** split into 6 submodules: types, crud, query, dead_code, test_map, related.
- **`cli/batch/handlers.rs` (2082 lines)** split into 6 submodules: search, graph, analysis, info, misc.
- **`search/scoring.rs` (1748 lines)** split into 5 submodules: config, name_match, note_boost, filter, candidate.

### Fixed
- **`--json` conflicts with `--format`** — clap `conflicts_with` prevents ambiguous `--json --format mermaid`.

### Dependencies
- Bumped `tree-sitter-rust` 0.24.0 → 0.24.1
- Bumped `tree-sitter-fsharp` 0.1.0 → 0.2.2
- Bumped `tracing-subscriber` 0.3.22 → 0.3.23
- Bumped `toml` 1.0.6 → 1.0.7
- Bumped `clap_complete` 4.5.66 → 4.6.0

## [1.3.1] - 2026-03-22

### Added
- **Hard eval supports local LoRA models** — `tests/model_eval.rs` resolves local directory paths, `local_lora_models()` auto-discovers v5/v7 from `~/training-data/`.

### Changed
- **Default embedding model upgraded to LoRA v7** on HuggingFace. GIST+Matryoshka+hard negatives: 0.707 CSN NDCG@10 (+2.4pp vs v5), 49.19 CoIR overall (nearly matches base E5). ONNX exported as opset 11 for CUDA EP compatibility.

### Fixed
- **FK constraint failure on fresh index** — `chunk_calls` were inserted before all chunks were committed across embedding batches, violating the `calls.caller_id` foreign key. Now deferred until all chunks are in the DB (same pattern as type_edges fix in v1.3.0). Also fixes silent call data loss on multi-file batches.

## [1.3.0] - 2026-03-21

### Added
- **`--improve-all` flag** for `cqs index --improve-docs` — regenerates doc comments for all callable functions, not just undocumented ones. Skips test functions (`#[test]`, `test_` prefix) and non-source files.
- **`is_source_file()` filter** — prevents doc comment injection into markdown, TOML, YAML, and config files.
- **`docs/research-log.md`** — consolidated research log with all 11 embedding experiments, verified CoIR metrics, and leaderboard analysis.

### Changed
- **Default embedding model upgraded to LoRA v5** (166k samples/1ep) on HuggingFace. +1.2pp CSN NDCG@10 and +1.4pp CosQA transfer vs previous v3 (50k/1ep).
- **Eval metrics improved**: Recall@1 90.9% → 92.7%, NDCG@10 0.951 → 0.965 (hard eval, DocFirst template) after doc comment enrichment of 629 functions across 182 source files.

### Fixed
- **Windows build**: removed `libc::EXDEV` reference in `doc_writer/rewriter.rs` that broke x86_64-pc-windows-msvc compilation. Replaced with platform-independent rename fallback.
- **Clippy**: `map_or` → `is_none_or`, empty lines between doc comments and attributes.

### Security
- Bumped `aws-lc-sys` 0.38.0 → 0.39.0 (CRL scope check logic error + X.509 name constraints bypass)
- Bumped `aws-lc-rs` 1.16.1 → 1.16.2

## [1.2.0] - 2026-03-21

### Added
- **SQ-8: `--improve-docs` flag** — LLM-generated doc comments for undocumented functions, written back to source files. Per-language DocWriter (11 explicit formats + `// ` default). Bottom-up insertion, decorator-aware, atomic writes.
- **SQ-11: Type-aware embeddings** — full function signatures appended to NL descriptions before embedding. +3.6pp R@1 on hard eval. TypeScript MRR +0.068.
- **SQ-12: `--hyde-queries` flag** — index-time HyDE query predictions via Batches API. LLM predicts 3-5 search queries per function, embedded alongside NL description.
- **`CQS_RERANKER_MODEL` env var** — configurable cross-encoder reranker model in `reranker.rs`.
- **`CQS_EMBEDDING_MODEL` env var** — configurable embedding model. Defaults to LoRA v3 fine-tuned model.
- **Reranker eval harness** — `test_hard_reranker_comparison` in model_eval.rs for before/after reranker measurement.
- **Weight sweep eval** — `test_weight_sweep` in model_eval.rs: 30-config parameter sweep for name_boost, keyword boost, RRF.
- **Type-aware embedding eval** — `test_type_aware_embeddings` in model_eval.rs.

### Changed
- **Default embedding model**: switched from `intfloat/e5-base-v2` to `jamie8johnson/e5-base-v2-code-search` (LoRA v3, +4.4pp CSN NDCG@10 on CoIR benchmark). Override with `CQS_EMBEDDING_MODEL=intfloat/e5-base-v2`.
- **LLM summary prompt**: changed from generic "Summarize this function" to discriminating "Describe what makes this function unique and distinguishable" (+16pp R@1 improvement). Requires re-running `--llm-summaries` to regenerate.
- **Schema v16**: composite PK `(content_hash, purpose)` on `llm_summaries` table. Supports `summary`, `doc_comment`, and `hyde` purposes.

### Fixed
- CSS `@media print { }` panic — `find('(')` failed when no parentheses present (#625)
- Doc rewriter `atomic_write` race condition — parallel tests with same PID produced identical temp file names (#628)

### Security
- Bumped `rustls-webpki` 0.103.9 → 0.103.10 (CRL scope check + X.509 name constraints bypass)

## [1.1.0] - 2026-03-19

### Breaking Changes
- **Schema v15:** Reindex required after upgrading (`cqs index --force`). Embeddings changed from 769-dim to 768-dim.
- **Notes removed from search results:** `--note-weight` and `--note-only` flags removed. Notes still boost code rankings and appear in `cqs read` output.
- **Search defaults to project-only:** Use `--include-refs` to include configured references. `--ref name` unchanged.

### Added
- `--include-refs` flag for cross-reference search
- `LlmConfig` with env/config overrides: `CQS_LLM_MODEL`, `CQS_API_BASE`, `CQS_LLM_MAX_TOKENS`
- `define_patterns!` macro for Pattern enum (EX-6)
- `capture` field in `define_chunk_types!` — generates `capture_name_to_chunk_type()` (EX-7)
- Shared test fixtures module (`src/test_helpers.rs`)
- Shared CLI/batch arg structs via `#[command(flatten)]` (EX-8)
- `ScoringConfig` struct consolidates 9 search scoring constants (EX-11)
- 49 new tests (TC-8 through TC-16): LLM store, migrations, dirty flag, pagination, notes, cache, open_readonly, watch events, extract_first_sentence
- CAGRA distance conversion now correct — vectors are truly unit-norm after sentiment removal

### Changed
- 768-dim embeddings (was 769 — sentiment dimension removed)
- `ProjectError`, `LlmError`, `ConfigError` typed error enums replace `anyhow` in library code
- `Store::open_with_config` deduplicates open/open_readonly
- Watch mode re-opens Store after reindex (clears stale OnceLock caches)
- BatchContext reference cache uses LRU eviction (cap 4)
- ORT provider functions gated on `target_os = "linux"` (was `cfg(unix)`)
- Notes use `normalize_path` for consistent storage
- 13 PathBuf fields use `serialize_path_normalized` for consistent JSON
- `search.rs` split into `search/` module (scoring.rs, query.rs, mod.rs)
- `embedder.rs` split into `embedder/` module (mod.rs, provider.rs)
- Enrichment pass extracted from `pipeline.rs` into `enrichment.rs`
- LLM summaries now use Batches API for throughput (no RPM limit, 50% discount) (#605)

### Fixed
- **Security:** API key leak via crafted batch_id (SEC-5/6), HNSW bincode OOM (SEC-7), atexit deadlock (SEC-8), batch OOM (SEC-9)
- **Robustness:** CJK byte slice panic (RB-7), SQL parser non-ASCII panic (RB-8/9), CAGRA k=0 (RB-11), byte_offset bounds (RB-10)
- **Data safety:** HNSW dirty flag crash protection (DS-11), atomic audit writes (DS-8), batch resume (DS-7/10)
- **Error handling:** Client::new returns Result (EH-8), pending batch errors logged (EH-10), swallowed errors surfaced (EH-11/16)
- **Observability:** 19 tracing sites converted to structured fields, 4 missing spans added
- **Algorithm:** waterfall_pack overshoot capped (AC-7)
- **Performance:** eq_ignore_ascii_case in search filter, content truncate before clone, MODEL.to_string() hoisted, integrity_check(1) replaces quick_check
- **Docs:** Schema version, --json vs --format json, SECURITY/PRIVACY for --llm-summaries, GPU section updated
- Dead code removed: NlTemplate variants, get_by_content_hash, get_enrichment_hash
- DeadConfidence/DeadConfidenceLevel consolidated into one enum
- Pattern enum and ChunkType checklist updated for macro consolidation
- CAGRA use-after-free on shape pointers — host ndarrays dropped while device tensors referenced them (#613)
- ORT CUDA provider path resolution — dladdr returns argv[0] on glibc, ORT falls back to CWD (#613)
- LLM batch resume on interrupt — persist batch_id in SQLite metadata, resume polling on restart (#613)

## [1.0.13] - 2026-03-16

### Added
- **SQ-6**: Optional LLM-generated function summaries via Claude Haiku API (`cqs index --llm-summaries`). One-sentence summaries prepended to NL descriptions for better code-vs-prose search ranking. Cached by content_hash — pay once per unique function body. Doc comment shortcut saves 30-50% on API costs. (#603)
- `llm_summaries` table (schema v14) for summary caching across rebuilds
- `content_hash` and `window_idx` fields on `ChunkSummary` for richer chunk metadata
- GC prunes orphan LLM summaries
- Enrichment pass incorporates LLM summaries into NL + hash when available

## [1.0.12] - 2026-03-16

### Added
- `cqs plan` command: classify tasks into 11 templates (language, bug fix, CLI flag, injection, etc.) via keyword scoring, run scout, output actionable checklist (#601)

### Fixed
- cqs-plan skill: stale file references (schema.rs → helpers.rs/migrations.rs, PRAGMA → metadata table)

## [1.0.11] - 2026-03-16

### Fixed
- **RT-DATA-4**: Notes file lock vs rename race — use separate `.lock` file that survives atomic renames (#599)
- **RT-DATA-2**: Enrichment idempotency — store blake3 hash of call context per chunk, skip re-enrichment when unchanged (#599)
- **RT-DATA-6**: HNSW crash desync — dirty flag in SQLite metadata detects interrupted writes, falls back to brute-force until rebuild (#599)

### Added
- `where_to_add` pattern coverage for 43 languages across 10 family groups: C-like, JVM, .NET, dynamic, functional, data science, systems, Solidity, shell (#555, #599)
- Schema v13 migration: `enrichment_hash` column + `hnsw_dirty` metadata key

### Changed
- Roadmap refreshed: v1.0.10 header, 51 languages, schema v12→v13, red team accepted findings section, missing injection entries

## [1.0.10] - 2026-03-15

### Fixed
- **HNSW ID desync** on zero-vector skip — used `id_map.len()` instead of loop index (RT-DATA-1, high) (#596)
- **CQS_PDF_SCRIPT** now rejects non-.py extensions to prevent arbitrary script execution (RT-INJ-1) (#596)
- **Path traversal** in `read_context_lines` — validates paths containing `..` against project root (RT-FS-1/2) (#596)
- **Chat input** length capped at 1MB to match batch mode (RT-RES-1) (#596)

## [1.0.9] - 2026-03-15

### Changed
- NL descriptions now include filename stem for module-level discrimination (SQ-5). Generic stems (mod, index, lib, utils, helpers) are filtered (#594)

## [1.0.8] - 2026-03-15

### Fixed
- Enrichment pass: assert embedding count mismatch instead of silent truncation (#592)
- Enrichment pass: drain batch after successful store write, not before (#592)
- Enrichment pass: skip ambiguous function names (`new`, `parse`, `build`) to prevent caller merging across files (#592)
- Enrichment pass: progress bar cleanup on error via closure guard (#592)
- `update_embeddings_batch`: check `rows_affected` and log missing chunk IDs (#592)
- `ProgressStyle::template().unwrap()` → `.expect()` in enrichment pass (#592)

### Changed
- Renamed `callee_document_frequencies()` → `callee_caller_counts()` — name now matches return type (#592)
- `ENRICHMENT_PAGE_SIZE` promoted from `let` to `const` (#592)

### Removed
- Dead code: `replace_file_chunks` (100 lines, zero production callers) (#592)

### Added
- 4 unit tests for `generate_nl_with_call_context` (callers, IDF filtering, truncation, empty context) (#592)

## [1.0.7] - 2026-03-14

### Added
- Call-graph-enriched embeddings (SQ-4): two-pass indexing re-embeds chunks with caller/callee context after call graph is built (#590)
- IDF-based callee filtering suppresses high-frequency utility functions (>10% threshold) (#590)
- `update_embeddings_batch()` for lightweight embedding-only updates (#590)
- `chunks_paged()` cursor-based chunk iterator (#590)
- `callee_document_frequencies()` for IDF computation (#590)

## [1.0.6] - 2026-03-14

### Added
- NL description enrichment (SQ-2): struct/enum/class field names and directory-path context improve embedding discrimination in large corpora (+3.7pp R@1 on hard eval) (#588)
- Holdout eval infrastructure: 143-query eval set with stress eval against real codebases (3970 chunks) (#588)
- `rerank_with_passages` method on reranker for scoring against custom passage text (#588)

### Fixed
- Dead code warning in Make language definition (#588)

## [1.0.5] - 2026-03-13

### Added
- ASP.NET Web Forms support — 51st language. Parses C#/VB.NET in server script blocks and `<% %>` expressions (#584)
- Makefile shell injection — extracts shell commands from recipe bodies via Bash grammar (#584)
- Class NL enrichment — Class/Struct/Interface chunks include member method names in NL descriptions for better semantic search (#585)
- `is_name_like_query()` — detects NL vs identifier queries, gates name_boost to prevent harmful re-ranking (#585)
- `parent_type_name` column in chunks table — methods carry enclosing class/struct/impl name through to search results (#585)
- Schema v11→v12 migration for `parent_type_name` (#585)

### Changed
- Test function demotion strengthened: `IMPORTANCE_TEST` 0.90→0.70, `IMPORTANCE_PRIVATE` 0.95→0.80 (#585)

### Fixed
- CUDA 13 compatibility via `--no-default-features` for ort build (#583)
- sqlx slow statement threshold raised from 1s to 10s to reduce log noise (#583)
- Flaky HNSW safety test — assertions now check memory safety, not approximate recall (#586)

## [1.0.4] - 2026-03-13

### Fixed
- Release workflow: make ort CUDA/TensorRT features conditional on non-macOS targets
- Release workflow: drop x86_64-apple-darwin target (ort-sys has no prebuilt binaries; use `cargo install cqs`)
- Release workflow: upgrade macOS runner from macos-13 (deprecated) to macos-14

## [1.0.1] - 2026-03-13

v1.0.0 audit fixes — 97 findings across 14 categories, all resolved.

### Fixed
- 13 P1 crashes/security fixes: RwLock panic, FTS assertion crash, reranker stride=0 panic, zero-vector NaN, CHM/PDF command injection, schema migration guard (#576)
- 12 P2 high-impact fixes: dual-Store race, migration ordering, anyhow-in-library, placeholder dedup, HNSW index search for cross-index gather, waterfall budget overshoot (#576)
- 60 P3 code quality/API/perf fixes: type safety (ChunkType enums, FunctionRisk struct, ReviewNoteEntry), serde consistency, LazyLock SQL caching, streaming JSON count_vectors, single-pass FTS sanitizer (#576)
- Release workflow: upgrade Linux runner to ubuntu-24.04 for glibc 2.38+ ort compatibility (#576)

### Added
- 55+ unit tests covering previously untested branches: suggest high_risk, health untested_hotspots, review match_notes, impact-diff depth-0 exclusion, related find_related, convert module (#577)
- Property tests for FTS5 MATCH sanitizer (#576)

## [1.0.0] - 2026-03-12

First stable release. Schema v11 stable since 2026-02-15. Tested on 3 codebases (cqs, aveva, rust). 50 languages, 1534 tests, two full audits complete.

### Added
- Configurable `ef_search` parameter via config file (clamped 10-1000, default 100) (#556)
- One-time WSL advisory lock warning on NTFS mounts (#558)
- `_with_*` naming convention documented in CONTRIBUTING.md (#557)

### Fixed
- Atomic cross-device file writes for notes and project registry (#559)
- Removed dead `scout_with_resources` function (#557)
- Inlined dead `compute_hints_with_graph_depth` into `compute_hints_with_graph` (#557)

## [0.28.3] - 2026-03-08

### Changed
- Indexing pipeline uses `parse_file_all()` for single-pass extraction of chunks + type edges, eliminating double file I/O (#563)
- Watch mode uses incremental HNSW `insert_batch` for changed chunks instead of full rebuild, with periodic full rebuild every 100 inserts (#561)
- `build_hnsw_index` delegates to `build_hnsw_index_owned` (dedup)

### Fixed
- Forward compatibility with ort 2.0.0-rc.12+ generic `Error<T>` type (#551)

## [0.28.2] - 2026-03-08

### Added
- Vue language support (50 languages total)
- Parser integration tests for 9 languages: C#, F#, PowerShell, Scala, Ruby, Vue, Svelte, Razor, VB.NET
- Fenced block call-graph test (documents known limitation)
- 100 audit fixes across P1-P4 tiers (91 P1-P3 + 9 P4)
- Tracing spans for 49 store functions across calls.rs, types.rs, notes.rs, chunks.rs

### Changed
- FTS indexing now uses batch INSERT for better performance
- `count_stale_files` delegates to `list_stale_files` (dedup)
- `SearchResult`/`NoteSearchResult` use `#[serde(flatten)]` for consistent serialization
- Named constants for search scoring thresholds
- `search_across_projects` now uses rayon parallel iteration

### Fixed
- Replace panicking positional `row.get(N)` with safe `ChunkRow::from_row()` in search_by_names_batch
- `find_project_root` now has depth limit (20) to avoid walking to filesystem root
- `is_webhelp_dir` now rejects symlinks (security hardening)
- Token budget warning messages clarify min-1 guarantee

## [0.28.1] - 2026-03-07

### Fixed
- Switch `tree-sitter-razor` and `tree-sitter-vb-dotnet` from git deps to crates.io v0.1.0 — enables `cargo install cqs` without git access (PR #548)

## [0.28.0] - 2026-03-07

Recursive injection framework expansion — Svelte, Razor/CSHTML, VB.NET language support, plus PHP→HTML→JS/CSS recursive injection and Nix/HCL/LaTeX→code injection rules (46 → 49 languages).

### Added
- **Svelte language support** (`.svelte`) — `tree-sitter-svelte-next` grammar. JS/TS + CSS injection from `<script>` and `<style>` blocks. TypeScript detection via `lang`/`type` attributes. Reuses HTML's `detect_script_language` (PR #546)
- **Razor/CSHTML language support** (`.cshtml`, `.razor`) — `tris203/tree-sitter-razor` fork. Monolithic grammar parses C#, HTML, and Razor directives in a single tree. C# chunks from `@code`/`@functions` blocks, HTML heading/landmark extraction, JS/CSS injection via `_inner` content mode (PR #546)
- **VB.NET language support** (`.vb`) — `CodeAnt-AI/tree-sitter-vb-dotnet` fork. Classes, modules, structures, interfaces, enums, methods, properties, events, delegates. Full call graph and type references (PR #546)
- **`_inner` content mode** — injection framework extension for grammars where container nodes have no named content child (e.g., Razor's generic `element` node). Extracts bytes between first `>` and last `</` in source text (PR #546)
- **PHP→HTML→JS/CSS recursive injection** — depth limit 3. Two injection rules: `program/text` (leading HTML) + `text_interpolation/text` (HTML after `?>`). `content_scoped_lines` prevents container-spans-file problem (PR #546)
- **Nix→Bash injection** — `indented_string_expression` in shell contexts (buildPhase, installPhase, shellHook). `detect_nix_shell_context` checks parent binding name (PR #546)
- **HCL→Bash injection** — `heredoc_template` with shell identifiers (EOT, BASH, SHELL). `detect_heredoc_language` checks heredoc identifier (PR #546)
- **LaTeX→code injection** — `minted_environment` + `listing_environment`. Language detection from `\begin{minted}{python}` and `[language=Rust]` options (PR #546)
- `byte_offset_to_point()` helper for `_inner` content mode range calculation

## [0.27.0] - 2026-03-06

Multi-grammar parsing — HTML files extract real JS/CSS chunks from embedded `<script>` and `<style>` blocks. Full 14-category audit with 57 findings resolved.

### Added
- **Multi-grammar injection parsing** — HTML `<script>` blocks extract real JS/TS function chunks; `<style>` blocks extract CSS rule chunks. Powered by tree-sitter's `set_included_ranges()`. `InjectionRule` on `LanguageDef` makes this extensible to other host languages (PR #540)
- **`parse_file_all()` combined method** — single file read + tree-sitter parse returns chunks, calls, and type refs. Eliminates double-parse in watch mode incremental reindexing (PR #544)
- **`ParseAllResult` type alias** for combined parse results
- **`parse_injected_all()`** — combined injection method for chunks + relationships in one inner parse
- **`detect_script_language()`** — detects TypeScript from `<script lang="ts">` or `<script type="text/typescript">` attributes
- End-to-end integration test for HTML injection parsing
- Tests for malformed/unclosed `<script>` tags, `type="text/typescript"` detection

### Fixed
- 57 audit findings resolved across 4 PRs (#541-#544): deduplication, correctness, security, observability, documentation
- `capture_name_to_chunk_type()` shared helper eliminates duplicated capture-name-to-type mapping (P1)
- `walk_for_containers()` now owns its cursor — takes `Node` instead of `&mut TreeCursor` (P4)
- Injection range deduplication prevents duplicate chunks from shared container kinds (P3)
- `run_git_log_line_range()` validates colons in file paths to prevent git `-L` misparse (P3)
- Cross-phase coupling between `parse_file` and `parse_file_relationships` documented (P4)

### Changed
- Watch mode (`reindex_files`) uses `parse_file_all()` instead of separate `parse_file` + `parse_file_relationships` calls

## [0.26.0] - 2026-03-05

Language expansion Phase 2, Batch 3 — Solidity, CUDA, GLSL (43 → 46 languages).

### Added
- **Solidity language support** (`.sol`) — contracts (Class), interfaces, libraries (Module), structs, enums, functions, modifiers, events (Property), state variables, call graph
- **CUDA language support** (`.cu`, `.cuh`) — reuses C++ grammar with CUDA qualifier filtering (__global__, __device__, __host__). Full call graph, out-of-class methods, kernel launches
- **GLSL language support** (`.glsl`, `.vert`, `.frag`, `.geom`, `.comp`, `.tesc`, `.tese`) — reuses C grammar with GLSL qualifier filtering (uniform, varying, precision). Shader function extraction and call graph

### Fixed
- Unused variable warning in embedding batch iterator (newer Rust versions)

## [0.25.0] - 2026-03-05

Language expansion Phase 2, Batch 2 — Nix, Make, LaTeX (40 → 43 languages).

### Added
- **Nix language support** (`.nix`) — function bindings, attribute sets, recursive attribute sets, function application call graph
- **Make language support** (`.mk`, `.mak`) — rules/targets (Function), variable assignments (Property)
- **LaTeX language support** (`.tex`, `.sty`, `.cls`) — sections/chapters/subsections (Section), command definitions (Function), environments (Struct)

### Fixed
- Parser now recognizes `@section` capture in tree-sitter queries (was missing from capture type mapping)
- Switched from broken `tree-sitter-latex` crate (missing scanner.c) to working `codebook-tree-sitter-latex`

## [0.24.0] - 2026-03-05

Language expansion Phase 2, Batch 1 — HTML, JSON, XML, INI (36 → 40 languages).

### Added
- **HTML language support** (`.html`, `.htm`, `.xhtml`) — semantic element classification: headings (Section), landmarks (Section), script/style blocks (Module), id'd elements (Property), noise filtering
- **JSON language support** (`.json`, `.jsonc`) — top-level key-value pairs (Property)
- **XML language support** (`.xml`, `.xsl`, `.xsd`, `.svg`) — elements (Struct), processing instructions (Function)
- **INI language support** (`.ini`, `.cfg`) — sections (Module), settings (Property)

## [0.23.0] - 2026-03-05

Mass language expansion — 20 → 36 languages (+16). Four batches (#527–#531).

### Added
- **Protobuf language support** (`.proto`) — messages (Struct), services (Interface), RPCs (Method), enums, type references via `message_or_enum_type`
- **GraphQL language support** (`.graphql`) — object types, interfaces, enums, unions (TypeAlias), input types, scalars, directives (Macro), operations, fragments, type references via `named_type`
- **PHP language support** (`.php`) — classes, interfaces, traits, enums, functions, methods, properties, constants, call extraction (function/method/static/constructor), type references, return type extraction
- **Lua language support** (`.lua`) — functions, local functions, method definitions, table constructors, call extraction
- **Zig language support** (`.zig`) — functions, structs, enums, unions, error sets, test declarations
- **R language support** (`.r`, `.R`) — functions, S4 classes/generics/methods, R6 classes, formula assignments
- **YAML language support** (`.yaml`, `.yml`) — mapping keys, sequences, documents
- **TOML language support** (`.toml`) — tables, arrays of tables, key-value pairs
- **Elixir language support** (`.ex`, `.exs`) — functions (def/defp), modules (defmodule), protocols (Interface), implementations (Object), macros, guards, delegates, pipe call extraction
- **Erlang language support** (`.erl`, `.hrl`) — functions, modules, records (Struct), type aliases, opaque types, behaviours (Interface), callbacks, local and remote call extraction
- **Haskell language support** (`.hs`) — functions, data types (Enum), newtypes (Struct), type synonyms (TypeAlias), typeclasses (Trait), instances (Object), return type extraction, function application call extraction
- **OCaml language support** (`.ml`, `.mli`) — let bindings, type definitions (variant/record/alias), modules, function application via value_path
- **Julia language support** (`.jl`) — functions, structs, abstract types, modules, macros
- **Gleam language support** (`.gleam`) — functions, type definitions, type aliases, constants
- **CSS language support** (`.css`) — rule sets (Property), keyframes and media queries (Section)
- **Perl language support** (`.pl`, `.pm`) — subroutines, packages (Module), function/method calls

## [0.19.5] - 2026-03-04

Full 75-finding code audit completed (14 categories, 3 batches). All findings addressed — 62 fixed, 13 triaged as acceptable/informational/by-design.

### Changed
- **Lightweight HNSW candidate fetch (PF-5)** — two-phase search: fetch only IDs+scores from HNSW, then batch-load full chunks from SQLite. Reduces memory during search.
- **Pipeline channel tuning (RM-4)** — separate depths for parse (512, lightweight) vs embed (64, heavy vector data) channels. Was uniform 256.
- **Watch mtime pruning (RM-5)** — threshold-based pruning (1K/10K entries) instead of per-cycle `exists()` calls on every file.
- **Generic token packing (CQ-3)** — unified `token_pack_unified`/`token_pack_tagged` into single generic `token_pack_results`.
- **SearchParams struct (AD-5)** — `dispatch_search` takes a struct instead of 9 individual parameters.
- **Pipeline name extraction (EX-4)** — `extract_from_scout_groups` folded into `extract_from_standard_fields` with automatic nested extraction.

### Fixed
- **SQLite 999-param limit (RB-1)** — `fetch_candidates_by_ids_async`/`fetch_chunks_by_ids_async` batched to stay under SQLite bind limit.
- **JSON injection in empty results (AC-1)** — `emit_empty_results` now uses `serde_json::json!` instead of raw `format!`.
- **Index lock race (DS-2)** — `acquire_index_lock` truncate+write is now atomic via temp file rename.
- **Force-index data loss (DS-7)** — `cmd_index --force` now writes new DB before removing old.
- **FTS debug_assert in release (SEC-2)** — query safety validation promoted from `debug_assert` to runtime check.
- **Dead source/ module (CQ-1)** — removed ~250 lines of unused code.
- **HNSW save rollback (DS-1)** — partial rename during save now rolls back already-moved files.
- **Blame path separators (PB-3)** — backslash paths normalized to forward-slash for Windows git compatibility.
- **PATH search validation (SEC-5)** — `find_7z`/`find_python` now validate exit status, not just executability.
- 43 additional P3 fixes: tracing spans, error context, Serialize derives, filter tests, CHM/webhelp memory, file size guards, and more.

### Added
- Parser integration tests for Bash, HCL, Kotlin, Swift, and Objective-C (TC-1).
- 7 `NoteBoostIndex` unit tests (TC-2).
- 7 search filter set tests (TC-3).
- 4 `ChatHelper::complete` tests (TC-4).
- 6 pipeline tests (TC-6).

### Dependencies
- `hnsw_rs` 0.3.3 → 0.3.4
- `tree-sitter` 0.26.5 → 0.26.6
- `tree-sitter-bash` 0.23.3 → 0.25.1
- `serial_test` 3.3.1 → 3.4.0

## [0.19.4] - 2026-02-28

### Added
- **`cqs blame <function>`** — semantic git blame via `git log -L` on a function's line range. Shows who changed it, when, and why. Supports `--callers`, `--json`, `-n <depth>`. Works in CLI, batch, and pipeline modes.
- **`cqs chat`** — interactive REPL wrapping batch mode with rustyline. Tab completion, history persistence, meta-commands (help/exit/clear). Same commands and pipeline syntax as `cqs batch`.

### Fixed
- **normalize_path centralization** — consolidated 31 inline `normalize_path` call sites into a single `cqs::normalize_path()` in lib.rs (PB-3 audit item).

## [0.19.3] - 2026-02-28

Second 14-category audit completed (117 findings). 107 of 109 actionable findings fixed across 4 priority tiers.

### Fixed
- **SQLite URL injection** — unescaped `?`/`#` in paths could corrupt SQLite connection URLs (SEC-1)
- **NaN panics in batch REPL** — 6 `serde_json::to_string().unwrap()` sites panic on NaN scores; switched to `serialize_f32_safe` (RB-1, RB-5)
- **NaN ordering in onboard** — score-to-u64 cast produces garbage sort order; now uses `total_cmp` (RB-8)
- **diff_parse unwrap on external input** — `starts_with` guard followed by bare `unwrap` (RB-7)
- **Watch mode lock/atomicity regressions** — index lock and atomic chunk+call writes claimed fixed in v0.19.0 but code was never applied (DS-1, DS-2)
- **HNSW RRF skip** — HNSW-guided search path bypassed RRF fusion, producing different results than brute-force path (AC-1)
- **BFS depth overwrite** — deeper depth replaced shallower in gather BFS expansion (AC-2)
- **Onboard embedding-only search** — missing RRF hybrid, keyword matches invisible (AC-3)
- **Parent dedup reduces below limit** — parent deduplication ran after limit, shrinking result count (AC-4)
- **open_readonly skipped integrity check** — `PRAGMA quick_check` only ran on writable opens despite SECURITY.md claims (DOC-6/SEC-3)
- **Symlink traversal in convert** — `convert_directory` followed symlinks outside project root (SEC-4)
- **Notes cache staleness** — `OnceLock` notes cache never invalidated in long-lived `Store` (DS-3)
- **HNSW copy not atomic** — fallback copy on cross-device rename could lose index on crash (DS-5)
- **GC prune partial crash** — per-batch transactions left orphans on interruption (DS-6, DS-4)
- **Notes file lock on read-only fd** — exclusive lock acquired on read-only file descriptor (DS-7)
- **N+1 SELECT in upsert** — content hash snapshotting queried per-chunk instead of batch (PF-1)
- **Call graph loaded 15 times** — `get_call_graph` called repeatedly with no caching (PF-7)
- **50MB test chunk scan** — `find_test_chunks` LIKE content scan called 13 times per session (PF-10)
- **Pipeline per-chunk staleness** — `needs_reindex` checked per-chunk not per-file (PF-2)
- **Note boost O(n*m)** — note boost computed O(notes × mentions) per chunk in inner loop (PF-4)
- **Redundant test chunk loading** — `analyze_impact` loaded test chunks separately from callers (PF-6)
- **Stale docs** — lib.rs example wouldn't compile, CONTRIBUTING.md listed phantom files and implemented languages as "future work", README commands wrong (DOC-1 through DOC-7)
- **Platform path matching** — `path_matches_mention` and `find_stale_mentions` failed on backslash paths (PB-1, PB-7)
- **Dead code test divergence** — `find_dead_code` inline path filter diverged from `is_test_chunk` (PB-2)

### Changed
- **Pipeline refactored** — 458-line `run_index_pipeline` split into `parser_stage`, `gpu_embed_stage`, `cpu_embed_stage`, `store_stage` (~136 lines of orchestration) (CQ-2)
- **Search refactored** — extracted `build_filter_sql()` (pure SQL assembly) and `score_candidate()` (shared scoring) from `search_filtered`, with 14 unit tests (CQ-8)
- **GatherDirection clap ValueEnum** — raw string replaced with typed enum (AD-1)
- **Audit mode state typed** — `Option<String>` replaced with enum (AD-2)
- **CLI arg naming unified** — `Scout.task` → `Scout.query`, `Onboard.concept` → `Onboard.query` (AD-3)
- **Redundant --json flags removed** — 4 commands had both `--format` and `--json` (AD-4)
- **Unused `_cli` params removed** — 7 command handlers accepted unused `&Cli` (AD-5)
- **Placement API consolidated** — 4 `suggest_placement` variants collapsed to 2 with `PlacementOptions` (AD-7)
- **StoreError variants refined** — `Runtime` catch-all split into specific variants; `AnalysisError::Embedder` preserves typed error chain (AD-10, EH-6)
- **Reference resolution deduped** — `cmd_diff`/`cmd_drift` shared 30 lines of boilerplate, now `resolve_reference()` helper (AD-8)
- **Serialize derives added** — `ScoutResult`, `TaskResult`, `GatherResult`, and related types now derive `Serialize` (AD-6)
- **Entry/trait names language-driven** — `ENTRY_POINT_NAMES`/`TRAIT_METHOD_NAMES` constants replaced with `LanguageDef` fields across 20 languages (EX-3)
- **Pipeline constants self-maintaining** — `is_pipeable()` on `BatchCmd` replaces manual constant; name extraction key-agnostic (EX-5, EX-6)
- **Structural pattern hooks** — language-specific pattern dispatch via `LanguageDef` (EX-2)
- **Test heuristics connected to language system** — `is_test_chunk` uses language registry (EX-8)
- **Tracing spans added** — `Store::open`, `search_across_projects`, gather BFS, HNSW `build_batched`, `find_dead_code` now have entry spans and stats logging (OB-1 through OB-9)
- **Temp file entropy** — PID+nanos replaced with `RandomState`-based entropy at 5 sites (SEC-2)
- **FTS safety documented** — `sanitize_fts_query` ordering invariant documented with `debug_assert` guards (SEC-5)
- **Impact degraded flag** — `ImpactResult.degraded` propagates batch name search failures (EH-7)
- **`SearchFilter::new()` removed** — duplicated `Default::default()` (AD-9)
- **HNSW extensions centralized** — single `HNSW_EXTENSIONS` constant replaces mismatched duplicates (CQ-3)
- **Reference lookup deduped** — `resolve_and_open_reference()` replaces 6-site boilerplate (CQ-5)
- **GatheredChunk From impl** — replaces 4 repeated 11-field constructions (CQ-7)
- **Dead code refactored** — 233-line function split into phases with named structs (CQ-6)
- **Watch mode refactored** — 9 indent levels flattened, embedder init deduped (CQ-4)
- **Query command deduped** — 5 repeated code paths consolidated (CQ-1)
- **Store mmap documented** — 256MB × 4 connection pool virtual address reservation explained (RM-4)
- **HNSW id map BufReader** — `count_vectors` uses buffered read instead of loading entire id map (RM-1)
- **Lightweight test chunk query** — `find_test_chunk_names_async()` avoids loading full `ChunkSummary` (RM-3)
- **merge_results truncate-first** — hash dedup runs on truncated results, not full set (RM-5)
- **Batch embedder idle timeout** — `BatchContext` releases embedder/reranker after inactivity (RM-6)
- **Gather/scout shared resources** — `_with_resources` variants avoid reloading call graph per call (RM-2)

### Added
- **14 new unit tests for `build_filter_sql`** — pure SQL assembly tested without database
- **`resolve_index_dir` tests** — 3 tests for `.cq` → `.cqs` migration
- **`enumerate_files` tests** — 2 tests for file enumeration
- **Batch boundary test** — 950-origin staleness check test
- **Review note matching test** — review diff tested with actual notes
- **Placement integration test** — `tests/where_test.rs`
- **Cross-project search tests** — `search_across_projects` test coverage
- **Schema migration test** — v10→v11 migration executed in tests
- **HNSW RRF behavior test** — verifies HNSW path produces same results as brute-force
- **Notes indexing test** — `index_notes` test coverage

## [0.19.2] - 2026-02-27

### Fixed
- **BFS duplicate expansion** — `bfs_expand` in `gather` revisited nodes when called with overlapping seeds. Added `HashSet<String>` visited set.
- **HNSW adaptive ef_search** — hardcoded `EF_SEARCH` candidate multiplier was suboptimal for varying index sizes. Now scales: `EF_SEARCH.max(k * 2).min(index_size.max(EF_SEARCH))`.
- **CLI error context sweep** — added `.context("Failed to ...")` on store operations across 10 CLI command files (stats, dead, graph, context, gc, trace, test_map, deps, index, query).

### Changed
- **Multi-row INSERT batching** — `upsert_chunks_batch`, `replace_file_chunks`, and `upsert_chunks_and_calls` now use `QueryBuilder::push_values` for multi-row INSERT in batches of 55 (55×18=990 < SQLite 999 param limit). Fewer round-trips for large chunk sets.
- **FTS skip on unchanged content** — `replace_file_chunks` snapshots content hashes before INSERT and skips FTS normalization for chunks whose `content_hash` didn't change. Reduces reindex cost for files with few modified functions.
- **Typed batch output** — new `ChunkOutput` struct with `#[derive(Serialize)]` replaces manual `serde_json::json!` assembly in batch handlers. Path normalization extracted to `normalize_path()` helper.
- **Pipeline `extract_names` refactored** — monolithic function split into `extract_from_bare_array`, `extract_from_standard_fields`, and `extract_from_scout_groups`.
- **Reference search typed errors** — `search_reference()` and `search_reference_by_name()` return `Result<_, StoreError>` instead of `anyhow::Result`.
- **Parallel reference loading** — `load_references()` uses `rayon::par_iter()` for concurrent Store+HNSW loading.
- **Config validation consolidated** — extracted `Config::validate(&mut self)` method, single `tracing::debug!(?merged)` log replaces per-field debug logging.

## [0.19.1] - 2026-02-27

### Fixed
- **NaN-safe sorting** — replaced 11 `partial_cmp().unwrap_or(Equal)` sites with `f32::total_cmp()` across drift, gather, onboard, search, project, reranker, reference, and CLI token budgeting. NaN scores no longer corrupt sort order.
- **UTF-8 panic in `first_sentence_or_truncate`** — `doc[..150]` can split multibyte codepoints. Now uses `floor_char_boundary(150)` before byte-slicing.
- **Predictable temp file names** — `config.rs` and `audit.rs` used fixed `"toml.tmp"` / `"json.tmp"` names. Now uses PID+timestamp suffix (matches existing `note.rs`/`project.rs` pattern).
- **SQLite 999-parameter limit** — `check_origins_stale` built unbounded `IN (?)` clauses. Now batched in groups of 900.
- **Duplicate call graph edges** — `get_call_graph` query missing `DISTINCT`, returning duplicate rows.
- **Redundant per-chunk FTS DELETE** — `replace_file_chunks` did per-chunk `DELETE FROM chunks_fts` inside loop after already bulk-deleting all FTS entries for the origin.
- **Batch REPL broken pipe** — `let _ = writeln!()` silently swallowed broken pipe errors. Now breaks the REPL loop on write failure.
- **Store::open error context** — bare `?` replaced with path-annotated error message.
- **GC stale count error** — `unwrap_or((0,0))` replaced with `tracing::warn!` on failure.
- **Doc syntax** — `cqs diff --source <ref>` corrected to `cqs diff <ref>` in CLAUDE.md, README.md, and bootstrap skill.

### Changed
- **`define_chunk_types!` macro** — replaces 4 manual match blocks for ChunkType Display/FromStr/error messages. Same pattern as existing `define_languages!`.
- **HealthReport Serialize** — added `#[derive(Serialize)]` chain through `HealthReport`, `IndexStats`, `Language`, `ChunkType`, and new `Hotspot` struct. Eliminated ~50 lines of hand-assembled JSON in CLI and batch handlers.
- **CLI/batch dedup for `explain` and `context`** — extracted shared `pub(crate)` core functions (`build_explain_data`, `build_compact_data`, `build_full_data`). Net -284 lines.
- **`semantic_diff` memory batching** — embedding loading changed from all-at-once to batches of 1000 pairs. Peak memory reduced from ~240MB to ~9MB for 20k-pair diffs.
- **Embedder validation** — `embed_batch_inner` now validates `seq_len` and total data length before ONNX inference.
- **Pipeline timing** — indexing pipeline now logs total elapsed time.
- **Watch mode locking** — reindex cycles acquire index lock via `try_lock()`, skip if already locked. Chunk and call graph writes use `upsert_chunks_and_calls()` for atomic transactions.

## [0.19.0] - 2026-02-26

### Added
- **Bash/Shell language support** — 16th language. Tree-sitter parsing for functions and command calls. Behind `lang-bash` feature flag (enabled by default).
- **HCL/Terraform language support** — 17th language. Tree-sitter parsing for resources, data sources, variables, outputs, modules, and providers. Qualified naming support (e.g., `aws_instance.web`). Call graph extraction (HCL built-in function calls like `lookup`, `format`, `toset`). Behind `lang-hcl` feature flag (enabled by default).
- **Kotlin language support** — 18th language. Tree-sitter parsing for classes, interfaces, enum classes, objects, functions, properties, type aliases. Call graph extraction (function calls + property access). Type dependency extraction (parameter types, return types, property types, inheritance, interface implementation). Behind `lang-kotlin` feature flag (enabled by default).
- **Swift language support** — 19th language. Tree-sitter parsing for classes, structs, enums, actors, protocols, extensions, functions, type aliases. Call graph extraction (function calls + property access + method calls). Type dependency extraction (parameter types, return types, property types, conformances). Behind `lang-swift` feature flag (enabled by default).
- **Objective-C language support** — 20th language. Tree-sitter parsing for class interfaces, protocols, methods, properties, C functions. Call graph extraction (message sends + C function calls). Behind `lang-objc` feature flag (enabled by default).
- **`post_process_chunk` hook on LanguageDef** — optional field for language-specific chunk reclassification (used by HCL for qualified naming; Kotlin for interface/enum reclassification; Swift for struct/enum/actor/extension reclassification).

### Fixed
- **Flaky `test_search_returns_results` CLI test** — relaxed assertion from checking specific function name to checking that results are returned. Embedding similarity between `add` and `subtract` functions is too close for deterministic ordering across CPU/GPU.

### Dependencies
- tree-sitter-bash 0.23 (new), tree-sitter-hcl 1.1 (new), tree-sitter-kotlin-ng 1.1 (new), tree-sitter-swift 0.7 (new), tree-sitter-objc 3.0 (new)

## [0.18.0] - 2026-02-26

### Added
- **C++ language support** (#492) — 15th language. Tree-sitter parsing for classes, structs, unions, enums (including `enum class`), namespaces, functions, inline methods, out-of-class methods (`Class::method`), destructors, concepts (C++20), type aliases (`using`/`typedef`), preprocessor macros and constants. Call graph extraction (direct, member, qualified, template function calls, `new` expressions). Type dependency extraction (parameters, return types, fields, base classes, template arguments). Out-of-class method inference via `extract_qualified_method` infrastructure. Behind `lang-cpp` feature flag (enabled by default).
- **`extract_qualified_method` on LanguageDef** — new optional field for languages where methods can be defined outside their class body (C++ `void Foo::bar() {}`). Infers `ChunkType::Method` + `parent_type_name` from the function's own declarator before parent-walking.

### Dependencies
- tree-sitter-cpp 0.23 (new)

## [0.17.0] - 2026-02-26

### Added
- **Scala language support** — 13th language. Tree-sitter parsing for classes, objects, traits, enums (Scala 3), functions, val/var bindings, and type aliases. Call graph extraction (function calls + field expression calls). Type dependency extraction (parameter types, return types, field types, extends clauses, generic type arguments). Behind `lang-scala` feature flag (enabled by default).
- **Ruby language support** — 14th language. Tree-sitter parsing for classes, modules, methods, and singleton methods. Call graph extraction. Behind `lang-ruby` feature flag (enabled by default).
- **ChunkType variants: Object, TypeAlias** — `Object` for Scala singleton objects, `TypeAlias` for Scala `type X = Y` definitions. Neither is callable.
- **SignatureStyle::FirstLine** — new signature extraction mode for Ruby (no `{` or `:` delimiter, extracts up to first newline).
- **TypeAlias backfill** — added TypeAlias capture to 5 existing languages: Rust (`type Foo = Bar`), TypeScript (`type Foo = ...`), Go (`type MyInt int`, `type Foo = int`), C (`typedef` — was incorrectly captured as Constant), F# (`type Foo = int -> string`).
- **C capture gaps filled** — `#define` constants (→ Constant), `#define(...)` function macros (→ Macro), `union` (→ Struct).
- **SQL capture gaps filled** — `CREATE TABLE` (→ Struct), `CREATE TYPE` (→ TypeAlias), `CREATE VIEW` reclassified from Constant to Function (named query).
- **Java capture gaps filled** — annotation types `@interface` (→ Interface), class fields (→ Property).
- **TypeScript namespace** — `namespace Foo { }` now captured as Module.
- **Ruby constants** — `CONSTANT = value` assignments now captured as Constant.

### Dependencies
- tree-sitter-scala 0.24 (new), tree-sitter-ruby 0.23 (new)

## [0.16.0] - 2026-02-26

### Added
- **F# language support** (#487) — 11th language. Tree-sitter parsing for functions, records, discriminated unions, classes, interfaces, modules, and members. Call graph extraction (function application + dot access). Type dependency extraction (record fields, parameter types, inheritance, interface implementation). Behind `lang-fsharp` feature flag (enabled by default).
- **PowerShell language support** (#487) — 12th language. Tree-sitter parsing for functions, classes, methods, properties, and enums. Call graph extraction (command calls, .NET method invocations, member access). Behind `lang-powershell` feature flag (enabled by default).
- **ChunkType variant: Module** — new chunk type for F# modules (not callable). Infrastructure for future Ruby/Elixir module support.

### Dependencies
- tree-sitter-fsharp 0.1.0 (new), tree-sitter-powershell 0.26.3 (new)

## [0.15.0] - 2026-02-25

### Added
- **C# language support** (#484) — 10th language. Tree-sitter parsing for classes, structs, records, interfaces, enums, methods, constructors, properties, delegates, events, and local functions. Call graph extraction (invocations + object creation). Type dependency extraction (base types, generic args, parameter/return types, property types). Behind `lang-csharp` feature flag (enabled by default).
- **ChunkType variants: Property, Delegate, Event** — new chunk types for C# (and future languages). `callable_sql_list()` replaces hardcoded SQL `IN` clauses. `is_callable()` method for type-safe callable checks.
- **Per-language `common_types`** — each LanguageDef now carries its own common type set. Runtime union replaces global hardcoded list. Enables language-specific type filtering in focused reads.
- **Data-driven container extraction** — `container_body_kinds` and `extract_container_name` on LanguageDef replace per-language match arms. Adding a language no longer requires editing the container extraction logic.
- **Score improvements moonshot** (#480) — pipeline eval harness, sub-function demotion in NL descriptions, NL template experiments. Production template switched Standard → Compact (+3.6% R@1 on hard eval).

### Changed
- **Skill consolidation** (#482) — consolidated 35 thin cqs-* skill wrappers into unified `/cqs` dispatcher (48 → 14 skills).

### Fixed
- **hf-hub reverted to 0.4.3** (#483) — 0.5.0 broke model downloads.

### Dependencies
- clap 4.5.58 → 4.5.60, toml 1.0.1 → 1.0.3, anyhow 1.0.101 → 1.0.102, chrono 0.4.43 → 0.4.44

## [0.14.1] - 2026-02-22

### Fixed
- **61-finding audit: P1-P4 fixes across 3 PRs** (#470, #471, #472) — 14-category code audit with red team adversarial review. P1+P2: 18 fixes (task CLI hardening, HNSW search bounds, impact format safety, gather depth guards). P3: 25 fixes (scout gap detection refactor, search edge cases, note locking, reference validation). P4: 18 fixes (batch pipeline fan-out cap, GC HNSW cleanup, embedding dimension warning, extensibility constants).
- **Flaky HNSW tests** — relaxed exact top-1 assertions to top-k contains for approximate nearest neighbor tests (#473).
- **`Embedding::new()` false positive** — dimension warning no longer fires on 768-dim pre-sentiment intermediate embeddings (#473).
- **Command listing sync** — added missing commands (task, health, suggest, convert, ref, project, review) across README, CLAUDE.md, audit skill, red-team skill, and bootstrap skill (#473).

### Added
- **Red team audit skill** (`.claude/skills/red-team/`) — reusable `/red-team` skill for adversarial security audits with 4 categories: input injection, filesystem boundary violations, adversarial robustness, silent data corruption (#472).

## [0.14.0] - 2026-02-22

### Added
- **`cqs task "description"`** (Phase 3 Moonshot) — single-call implementation brief combining scout + gather + impact + placement + notes. Loads call graph and test chunks once instead of per-phase. Waterfall token budgeting across 5 sections (scout 15%, code 50%, impact 15%, placement 10%, notes 10%). Supports `--tokens`, `--json`, `-n`, and batch mode. 9 new tests.
- **NDCG@10 and Recall@10 metrics** in eval harness and README. E5-base-v2: 0.951 NDCG@10, 98.2% Recall@10. Performance benchmarks: 45ms hot-path search (p50), 22 QPS batch throughput, 36s index build for 203 files.
- **RAG Efficiency section** in README — measured 17-41x token reduction vs full file reads using `gather` and `task` with token budgeting.

### Fixed
- **Scout ModifyTarget classification** — replaced hardcoded 0.5 threshold (broken on RRF scores ~0.01-0.03) with automatic gap detection. Finds largest relative score gap to separate modify targets from dependencies. Scale-independent, no tuning parameter. 6 new tests.
- **Batch `--tokens` wiring** — all batch handlers now correctly pass through token budget parameter (#467).

## [0.13.1] - 2026-02-21

### Changed
- **Split `batch.rs` into `batch/` directory** — 2844-line monolith split into 4 focused files: `mod.rs` (BatchContext, main loop), `commands.rs` (parsing, dispatch), `handlers.rs` (23 handler functions), `pipeline.rs` (pipe chaining, fan-out). No behavior change.

### Fixed
- **P4 audit: 18 test + extensibility + resource management fixes** (#463) — 6 test improvements (edge cases, property-based tests for health/suggest/onboard), 9 extensibility enhancements (language registry, parser config), 3 resource management fixes (drop ordering, cleanup).
- **CQ-8/CQ-9 read dedup** — extracted shared read logic (`validate_and_read_file`, `build_file_note_header`, `build_focused_output`) into `commands/read.rs`. Both CLI and batch read paths call shared core, eliminating ~200 lines of duplicated code.
- **SECURITY.md** — path traversal code snippet updated to reflect `dunce::canonicalize` usage.

## [0.13.0] - 2026-02-21

### Added
- **`cqs onboard "concept"`** (Phase 2b) — guided codebase tour that replaces the manual scout → read → callers → callees → test-map → explain workflow with a single command. Returns an ordered reading list: entry point → call chain (BFS callees) → callers → key types → tests. Supports `--depth` (1-5), `--tokens` budget, `--json`, and batch mode. Entry point selection prefers callable types (Function/Method) with call graph connections over structs/enums. 12 new tests.
- **Auto-stale note detection** (Phase 2c) — 4th detector in `cqs suggest` identifies notes with stale mentions (deleted files, renamed functions). Classifies mentions as file-like, symbol-like, or concept (skipped). File mentions checked via filesystem, symbol mentions batch-checked via `search_by_names_batch()`. `notes list --check` flag annotates notes with stale mentions inline. 7 new tests.
- **`cqs drift <reference>`** (Phase 2d) — semantic change detection between reference snapshots. Wraps `semantic_diff()` to surface functions that changed semantically, sorted by drift magnitude (most changed first). Supports `--min-drift` threshold, `--lang` filter, `--limit`, `--json`, and batch mode. 4 new tests.

### Fixed
- **P1 audit: 12 security + correctness fixes** (#459) — Store path traversal guard, batch input size limit, reference store opened read-only, BFS unbounded iteration guards, error propagation on Store::open/note queries, delete-by-file scoped to chunk IDs, type edge upsert uses chunk-level scope.
- **P2 audit: 18 caching + quality fixes** (#460) — BatchContext caching for call graph, config, reranker, file set, audit state, and notes (6 fixes). N+1 query elimination in `get_ref`/`dispatch_drift` (2). Code quality: dedup removal, COMMON_TYPES consolidation (2). API design: TypeUsage struct, onboard error propagation, chunk_type Display consistency, float param validation (4). Robustness: NaN/Infinity rejection on float params (4). Renamed `gpu-search` feature flag to `gpu-index`.
- **P3 audit: 31 docs + observability + robustness fixes** (#461) — Documentation: README, CONTRIBUTING, CHANGELOG, ROADMAP, SECURITY accuracy (9). Error handling: `.context()` on Store::open/embed_query, debug→warn for staleness errors (5). Observability: tracing spans for pipeline/windowing/embed threads, batch error counter (4). API design: Debug+Clone on TypeGraph/ResolvedTarget, Serialize on Note, drift type re-exports, TypeEdgeKind enum replaces stringly-typed edge_kind (5). Robustness: onboard depth clamp, search_by_name limit clamp, usize for type graph constants (5). Performance: Cow<str> in strip_markdown_noise, PathBuf forward-slash serialization (3). Test coverage: TypeEdgeKind round-trip test, staleness assertion (2).

## [0.12.12] - 2026-02-18

### Added
- **Parent type context in NL descriptions** — methods now include their parent struct/class/trait name in natural language descriptions (e.g., `should_allow()` on `CircuitBreaker` gets "circuit breaker method"). Extraction covers 6 languages: Rust impl/trait, Python class, JS/TS/Java class, Go method receiver. 15 new tests (11 parser + 4 NL).
- **Hard eval suite** — 55 confusable queries across 5 languages with 15 similar functions per language (6 sort variants, 4 validators, resilience patterns). Pre-embedded query deduplication eliminates 4x redundant ONNX inference.

### Changed
- **Docs repositioned as code intelligence + RAG** — README, Cargo.toml description and keywords updated to lead with code intelligence, call graphs, and context assembly rather than just "code search".

### Improved
- **Retrieval quality** — E5-base-v2 Recall@1 improved from 86% to 90.9%, MRR from 0.885 to 0.941 on hard eval. Perfect MRR (1.0) on Rust, Python, and Go. Confirmed E5 beats jina-v2-base-code (80.0% R@1, 0.863 MRR).

## [0.12.11] - 2026-02-15

### Added
- **Type extraction parser** (Phase 1a Step 1) — tree-sitter type queries for 6 languages (Rust, Python, TypeScript, Go, Java, C). Extracts struct/enum/class/interface/typedef definitions and function parameter/return type references. `TypeEdgeKind` enum (Uses, Returns, Field, Impl, Bound, Alias). `parse_file_relationships()` returns both call sites and type refs. 19 new tests.
- **Type edge storage and `cqs deps` command** (Phase 1a Step 2) — schema v11 adds `type_edges` table with FK CASCADE. 10 store methods (upsert, query, batch, stats, graph, prune). `cqs deps <type>` shows who uses a type; `cqs deps --reverse <fn>` shows what types a function uses. Batch mode support with pipeline compatibility. GC prunes orphan type edges. Stats includes type graph counts. 17 new tests.

### Fixed
- **Removed 100-line chunk limit** — `parse_file()` silently dropped any chunk over 100 lines, causing 52 functions (including `cmd_index`, `search_filtered`, `cmd_query`) to be entirely absent from the index. Large chunks are now handled by token-based windowing (480 tokens, 64 overlap) in the pipeline instead.
- **Added windowing to watch mode** — `cqs watch` sent raw chunks directly to the embedder without windowing, silently truncating functions exceeding 480 tokens. Now uses the same `apply_windowing()` as the full indexing pipeline.

## [0.12.10] - 2026-02-14

### Added
- **Pipeline syntax for `cqs batch`** — chain commands where upstream names feed downstream via fan-out: `search "error" | callers | test-map`. Quote-safe parsing (shell_words tokenize first, split by `|` token). 7 pipeable downstream commands: callers, callees, explain, similar, impact, test-map, related. Fan-out capped at 50 names per stage. Pipeline envelope output with `_input`/`data` wrappers. No new dependencies.
- 17 unit tests (name extraction, pipeable check, token splitting) + 7 integration tests (pipeline end-to-end).

## [0.12.9] - 2026-02-14

### Added
- **`cqs batch` command** — persistent Store batch mode. Reads commands from stdin, outputs compact JSONL. Amortizes ~100ms Store open and ~500ms Embedder ONNX init across N commands. 13 commands supported: search, callers, callees, explain, similar, gather, impact, test-map, trace, dead, related, context, stats. Lazy Embedder and HNSW/CAGRA vector index via `OnceLock` — built on first use, cached for session. Reference indexes cached in `RefCell<HashMap>`. `dispatch()` function is the seam for step 3 (REPL).
- `shell-words` dependency for batch command tokenization.
- 10 unit tests (command parsing) + 9 integration tests (batch CLI pipeline).

### Changed
- **`ChunkSummary` type consistency** — `ChunkIdentity`, `LightChunk`, `GatheredChunk` now use `Language`/`ChunkType` enums instead of `String`. Parse boundary at SQL read layer.
- **`DocFormat` registry table** — static `FORMAT_TABLE` replaces 4 match blocks; adding a new document format now requires 3 changes instead of 6.

## [0.12.8] - 2026-02-14

### Added
- **`cqs health` command** — codebase quality snapshot composing stats, dead code, staleness, hotspot analysis, and untested hotspot detection. Graceful degradation (individual sub-queries fail without aborting). `--json` supported.
- **`cqs suggest` command** — auto-detect note-worthy patterns (dead code clusters, untested hotspots, high-risk functions) and suggest notes. Dry-run by default, `--apply` to add, `--json` for structured output. Deduplicates against existing notes.

### Changed
- **`Store::search()` renamed to `search_embedding_only()`** — prevents accidental use of raw cosine similarity without RRF hybrid. All user-facing search should use `search_filtered()`.

### Fixed
- **Convert TOCTOU race (#410)** — replaced check-then-write with atomic `create_new` to prevent race condition in output file creation.
- **`gather_cross_index` test coverage (#414)** — added 4 integration tests (basic bridging, empty ref, ref-only, limit).

## [0.12.7] - 2026-02-13

### Added
- **`cqs ci` command** — CI pipeline analysis composing review_diff + dead code detection + gate evaluation. `--gate high|medium|off` controls failure threshold (exit code 3 on fail). `--base`, `--stdin`, `--json`, `--tokens` supported.
- **`--rerank` flag** — Cross-encoder re-ranking for query results. Second-pass scoring with `cross-encoder/ms-marco-MiniLM-L-6-v2` reorders top results for higher accuracy. Over-retrieves 4x then re-scores. Works with no-ref and `--ref` scoped queries. Warns and skips for multi-index search (incompatible score scales).

## [0.12.6] - 2026-02-13

### Fixed
- **`score_name_match` empty query bug** (#415): Empty query returned 0.9 (prefix match) instead of 0.0.
- **`PathBuf::from("")` cosmetic** (#417): Replaced `unwrap_or_default()` with conditional push in PDF script lookup.
- **Unicode lowercasing** (#418): `title_to_filename` now uses `to_lowercase()` instead of `to_ascii_lowercase()`, properly handling non-ASCII characters.

### Added
- **`blast_radius` field on `RiskScore`** (#408): Based on caller count alone (Low 0-2, Medium 3-10, High >10). Unlike `risk_level`, does not decrease with test coverage. Displayed when it differs from risk level.
- **`--format` option for `cqs review`** (#416): Parity with `impact`/`trace` commands. Accepts `text` or `json`. `--json` remains as alias. Mermaid returns an error (unsupported for review data model).
- **`test_file_suggestion` on `LanguageDef`** (#420): Data-driven test file path conventions per language, replacing hardcoded match in `suggest_test_file`.
- 14 new tests: 6 token_pack, 2 score_name_match, 3 blast_radius, 1 unicode naming, 2 risk scoring.

### Changed
- **Token packing JSON overhead** (#409): `token_pack` now accepts `json_overhead_per_item` parameter. JSON output accounts for ~35 tokens per result for field names and metadata. Affects `query`, `gather`, `review`, `context`, `explain`, `scout` commands with `--tokens`.
- **Cross-index bridge parallelization** (#411): `gather --ref` bridge search uses `rayon::par_iter` instead of sequential loop.
- **Deduplicated `read_stdin`/`run_git_diff`** (#419): Moved to shared `commands/mod.rs` with tracing span on `run_git_diff`.
- **`WEBHELP_CONTENT_DIR` constant** (#413): Extracted from duplicated `"content"` string literals.

## [0.12.5] - 2026-02-13

### Fixed
- **Eliminated unsafe transmute in HNSW index loading** (#270): Replaced raw pointer + `transmute` + `ManuallyDrop` + manual `Drop` with `self_cell` crate for safe self-referential ownership. Zero transmute, zero ManuallyDrop, zero Box::from_raw remaining in `src/hnsw/`.

### Added
- 4 `--ref` CLI integration tests (TC-6): `query --ref`, `gather --ref`, ref-not-found error path, `ref list` verification.
- `self_cell` dependency (v1) for safe self-referential HNSW index management.

## [0.12.4] - 2026-02-13

### Fixed
- **v0.12.3 audit: 61 P1-P3 findings fixed** across 14 categories — security hardening, correctness bugs, N+1 query patterns, API types, algorithm fixes, test coverage, documentation.
- **Security**: Symlink escape protection in CHM/WebHelp walkdir (SEC-9/11), zip-slip containment with `dunce::canonicalize` (SEC-10), `CQS_PDF_SCRIPT` env var warning (SEC-8).
- **Correctness**: `score_name_match` 0.5 floor → 0.0 for non-matches (AC-13), reference stores opened read-only (DS-8/RM-11), `DiffTestInfo.via` per-function BFS attribution (AC-16), `dunce::canonicalize` in convert overwrite guard (PB-11).
- **Performance**: 4 N+1 query patterns batched — transitive callers, suggest_tests, diff_impact, context (CQ-3/RM-14/PERF-13/PERF-12). `review_diff` single graph/test_chunks load (CQ-1/RM-10). SQLite batch inserts respect 999 variable limit (RB-15/16/DS-7). Batch tokenization (PERF-15).
- **Algorithm**: Gather BFS decay per-hop instead of exponential compounding (AC-14), expansion cap enforced per-neighbor (AC-18), snippet bounds check for windowed chunks (AC-19), context token packing by relevance (AC-21).
- **Conventions**: Safe `chars().next()` replacing `unwrap()` (EH-18/RB-12), `strip_prefix` replacing byte-index slicing (RB-14), `--tokens 0` rejected with error (RB-18), broadened copyright regex (EXT-19).

### Changed
- **Impact types**: Added `Debug`, `Clone`, `Serialize` derives and missing re-exports (AD-12/13).
- **CLI args**: `OutputFormat` and `DeadConfidenceLevel` are now `clap::ValueEnum` enums instead of stringly-typed (AD-17).
- **`RiskScore`**: Removed redundant `name` field (AD-18). Risk threshold constants `RISK_THRESHOLD_HIGH`/`MEDIUM` (EXT-13).
- **Review types**: Simplified to use impact types directly — `CallerEntry`/`TestEntry` replaced with `CallerDetail`/`DiffTestInfo` (CQ-4/AD-14).
- **Gather**: Shared BFS helpers (`bfs_expand`, `fetch_and_assemble`) deduplicate `gather`/`gather_cross_index` (CQ-2). Model compatibility check on cross-index gather (DS-10).
- **Generic `token_pack<T>`**: Replaces 5 inline packing loops across commands (EXT-15).
- **Reference search**: 4 functions → 2 with `apply_weight` param (CQ-5). `batch_count_query` deduplicates caller/callee counts (CQ-7). `finalize_output` deduplicates convert pipeline (CQ-6).
- **Observability**: Tracing spans added to `suggest_tests`, `compute_hints`, `cmd_query_name_only`. `.context()` on 7z spawn and fs operations. `LazyLock` for 6 cleaning regexes. `warnings` field in `ReviewResult`.
- **`cqs review --tokens`**: Token budgeting support added (EXT-18).

### Refactored
- **`impact.rs` split into `src/impact/` directory**: `mod.rs`, `types.rs`, `analysis.rs`, `diff.rs`, `bfs.rs`, `format.rs`, `hints.rs` (#402).

### Added
- 80 new tests: review_diff (5), reverse_bfs_multi (6), token budgeting (2), diff_impact e2e (3), score_name_match (4), plus integration tests.

## [0.12.3] - 2026-02-12

### Added
- **`cqs review`**: Comprehensive diff review — composes impact-diff + note matching + risk scoring + staleness check into a single structured payload. Supports `--base <ref>`, `--stdin`, `--json`. Text output with colored risk indicators.
- **Change risk scoring**: `compute_risk_batch()` and `find_hotspots()` in impact module. Formula: `score = caller_count * (1 - coverage)`. Three levels: High (>=5), Medium (>=2), Low (<2). Entry-point exception: 0 callers + 0 tests = Medium.
- **`cqs plan` skill**: Task planning with scout data and 5 task-type templates (feature, bugfix, refactor, migration, investigation).
- **`--ref` scoped search**: `cqs "query" --ref aveva` searches only the named reference index, skipping the project index. Returns raw scores (no weight attenuation). Works with `--name-only` and `--json`. Error on missing ref with `cqs ref list` hint.
- **`cqs gather --ref`**: Cross-index gather — seeds from a reference index, bridges into project code via embedding similarity, then BFS-expands via the project call graph. Returns both reference context and related project code in a single call.
- **`--tokens` token budgeting**: Greedy knapsack packing by score within a token budget, across 5 commands:
  - `cqs "query" --tokens 4000` — pack highest-scoring search results into budget
  - `cqs gather "query" --tokens 4000` — pack gathered chunks into budget
  - `cqs context file.rs --tokens 4000` — include chunk content within budget (full mode only)
  - `cqs explain func --tokens 3000` — include target + similar chunks' source code
  - `cqs scout "task" --tokens 8000` — fetch and include chunk content in dashboard
  - Token count and budget reported in both text and JSON output. JSON adds `token_count` and `token_budget` fields.
- **`cqs convert` command**: Convert PDF, HTML, CHM, web help sites, and Markdown documents to cleaned Markdown with kebab-case filenames. PDF via Python `pymupdf4llm`, HTML/CHM/web help via Rust `fast_html2md`, Markdown passthrough for cleaning and renaming.
- **Web help ingestion**: Auto-detects multi-page HTML help sites (AuthorIT, MadCap Flare) by `content/` subdirectory heuristic. Merges all pages into a single document.
- **Extensible cleaning rules**: Tag-based system (`aveva`, `pdf`, `generic`) for removing conversion artifacts. 7 rules ported from `scripts/clean_md.py`.
- **Collision-safe naming**: Title extraction (H1 → H2 → first line → filename), kebab-case conversion, source-stem and numeric disambiguation.
- **`convert` feature flag**: Optional dependencies (`fast_html2md`, `walkdir`) gated behind `convert` feature (enabled by default).

## [0.12.2] - 2026-02-12

### Added
- **`HnswIndex::insert_batch()`**: Incremental HNSW insertion on Owned variant for watch mode. Dimension validation, tracing, rejects Loaded variant with clear error.
- **`--min-confidence` flag for `cqs dead`**: Filter dead code results by confidence level (low/medium/high). Reduces false positive noise.
- **`DeadFunction` + `DeadConfidence`**: Confidence scoring for dead code detection — High (private, inactive file), Medium (private, active file), Low (method/dynamic dispatch).
- **`ENTRY_POINT_NAMES` exclusions**: Dead code analysis now excludes runtime entry points (main, init, handler, middleware, setup/teardown, test lifecycle hooks).
- **C, SQL, Markdown language arms** in `extract_patterns()` for `cqs where` placement suggestions.

### Fixed
- **Test detection unified**: `is_test_chunk()` replaces 3 divergent implementations (scout, impact, where_to_add) with a single function checking both name patterns and file paths.
- **Embedder `clear_session(&self)`**: Changed from `&mut self` via `Mutex<Option<Session>>`, enabling watch mode to free ~500MB ONNX session after 5 minutes idle.
- **Pipeline memory**: `file_batch_size` reduced from 100,000 to 5,000, bounding peak memory at ~25K chunks per batch.
- **HNSW error messages**: Checksum failure and load errors now include actionable guidance ("Run 'cqs index' to rebuild").
- **HNSW stale temp cleanup**: `load()` removes leftover `.tmp` directories from interrupted saves.
- **HNSW file locking**: Exclusive lock on save, shared lock on load via Rust 1.93 std file locking API. Prevents concurrent corruption.
- **Dead code false positives**: Expanded `TRAIT_METHOD_NAMES` with `new`, `build`, `builder`. Entry point exclusion list replaces hardcoded `main`-only check.

### Changed
- **`extract_patterns()` refactored**: `extract_imports()` and `detect_error_style()` helpers reduce per-language duplication.
- **`find_dead_code()` return type**: Now returns `Vec<DeadFunction>` (wrapping `ChunkSummary` + `DeadConfidence`) instead of `Vec<ChunkSummary>`.

## [0.12.1] - 2026-02-11

### Added
- **`--no-stale-check` flag**: Skip per-file staleness checks on slow filesystems (NFS, network mounts). Also configurable via `stale_check = false` in `.cqs.toml`.

### Fixed
- **Scout note matching precision**: `find_relevant_notes()` no longer produces false matches from reverse suffix comparison. Now requires path-component boundary matching (e.g., mention "search.rs" matches "src/search.rs" but not "nosearch.rs").

### Removed
- **`type_map` dead code**: Removed `LanguageDef.type_map` field and all per-language `TYPE_MAP` constants (never read, zero call sites).

## [0.12.0] - 2026-02-11

### Added
- **`cqs stale`**: New command to check index freshness. Lists files modified since last index and files in the index that no longer exist on disk. Supports `--json`, `--count-only`.
- **Proactive staleness warnings**: Search, explain, gather, and context commands now warn on stderr when results come from stale files. Suppressed with `-q`.
- **`cqs context --compact`**: Signatures-only TOC with caller/callee counts per chunk. One command to see what's in a file and how connected each piece is. Uses batch SQL queries (no N+1).
- **`cqs related <function>`**: Co-occurrence analysis — find functions that share callers, callees, or custom types with a target. Three dimensions for understanding what else needs review when touching code.
- **`cqs impact --suggest-tests`**: For each untested caller in impact analysis, suggests test name, file location (inline or new file), and naming pattern. Language-aware for Rust, Python, JS/TS, Java, Go.
- **`cqs where "description"`**: Placement suggestion — find the best file and insertion point for new code. Extracts local patterns (imports, error handling, naming convention, visibility, inline tests) for each suggested file.
- **`cqs scout "task"`**: Pre-investigation dashboard — single command replaces search → read → callers → tests → notes workflow. Groups results by file with signatures, caller/test counts, role classification, staleness, and relevant notes.
- **Bootstrap agent skills propagation**: Bootstrap template now instructs spawned agents to include cqs tool instructions in their prompts.

## [0.11.0] - 2026-02-11

### Added
- **Proactive hints** (#362): `cqs explain` and `cqs read --focus` now show caller count and test count for function/method chunks. JSON output includes `hints` object with `caller_count`, `test_count`, `no_callers`, `no_tests`.
- **`cqs impact-diff`** (#362): New command maps git diff hunks to indexed functions and runs aggregated impact analysis. Shows changed functions, affected callers, and tests to re-run. Supports `--base`, `--stdin`, `--json`.
- **Table-aware Markdown chunking** (#361): Markdown tables are chunked row-wise when exceeding 1500 characters. Parent retrieval via `--expand` flag.
- **Markdown RAG improvements** (#360): Richer embeddings with cross-document reference linking and heading hierarchy preservation.
- **`cqs-impact-diff` skill**: Agent skill for diff-aware impact analysis.

### Fixed
- **Suppress ort warning** (#363): Filter benign "nodes not assigned to preferred execution providers" warning from ONNX Runtime.
- **Double compute_hints in read.rs**: JSON mode was calling `compute_hints()` twice; now stores result and reuses.

## [0.10.2] - 2026-02-10

### Fixed
- **Stale MCP documentation**: Removed references to `cqs serve`, HTTP transport, and MCP setup from README, CONTRIBUTING, and PRIVACY. MCP server was removed in v0.10.0.

## [0.10.1] - 2026-02-10

### Added
- **CLI integration test harness** (#300): 27 new integration tests covering trace, impact, test-map, context, gather, explain, similar, audit-mode, notes, project, and read commands.
- **Embedding pipeline tests** (#344): 9 integration tests for document embedding, batch processing, determinism, and query vs document prefix differentiation.
- **Cross-store dedup** (#256): Reference search results deduplicated by content hash (blake3) — identical code from multiple indexes no longer appears twice.
- **Parallel reference search** (#257): Reference indexes searched concurrently via rayon instead of sequentially.
- **Streaming brute-force search** (#269): Cursor-based batching (5000 rows) replaces `fetch_all()` in brute-force path, reducing peak memory from O(total chunks) to O(batch size).
- **HNSW file size guards** (#303): Graph (500MB) and data (1GB) file size checks before deserialization prevent OOM on corrupted/malicious index files.
- **CAGRA OOM guard** (#302): 2GB allocation limit check before `Vec::with_capacity()` in GPU index building.

### Fixed
- **FTS5 injection defense-in-depth**: RRF search path now sanitizes FTS queries after normalization, closing a gap where special characters could reach MATCH.
- **HNSW checksum enforcement**: Missing checksum file now returns an error instead of silently loading unverified data.
- **Reference removal containment**: `ref remove` uses `dunce::canonicalize` + `starts_with` to verify deletion target is inside refs root directory.
- **Symlink reference rejection**: Symlink reference paths are skipped instead of loaded, preventing trust boundary bypass.
- **Display file size guard**: 10MB limit on files read for display, preventing accidental large file reads.
- **Config/notes size guards**: 1MB limit on config files, 10MB on notes files before `read_to_string`.
- **Similar command overflow**: `limit + 1` uses `saturating_add` to prevent overflow on `usize::MAX`.
- **Predictable temp file paths**: Notes temp files include PID suffix to prevent predictable path attacks.
- **Call graph edge cap**: 500K edge limit on call graph queries prevents unbounded memory on enormous codebases.
- **Trace depth validation**: `--max-depth` clamped to 1..50 via clap value parser.

## [0.10.0] - 2026-02-10

### Removed
- MCP server (`src/mcp/`, `cqs serve` command). All functionality available via CLI + skills.
- `cqs batch` command (was MCP-only, no CLI equivalent).
- Dependencies: axum, tower, tower-http, futures, tokio-stream, subtle, zeroize.
- Tokio slimmed from 6 features to 2 (`rt-multi-thread`, `time`).

### Changed
- `parse_duration()` moved from `src/mcp/validation.rs` to `src/audit.rs`.

## [0.9.9] - 2026-02-10

### Fixed
- **HNSW staleness in watch mode** (#236): Watch mode now rebuilds the HNSW index after reindexing changed files, so searches immediately find newly indexed code.
- **MCP server HNSW staleness** (#236): MCP server lazy-reloads the HNSW index when the on-disk checksum file changes, using mtime-based staleness detection.

### Changed
- **MSRV bumped to 1.93**: Minimum supported Rust version raised from 1.88 to 1.93.
- **Removed `fs4` dependency**: File locking now uses `std::fs::File::lock()` / `lock_shared()` / `try_lock()` (stable since Rust 1.89).
- **Removed custom `floor_char_boundary`**: Uses `str::floor_char_boundary()` from std (stable since Rust 1.91).
- **MSRV CI job**: New CI check validates compilation on the minimum supported Rust version.

## [0.9.8] - 2026-02-11

### Added
- **SQLite integrity check**: `PRAGMA quick_check` on every `Store::open()` catches B-tree corruption early with a clear `StoreError::Corruption` error.
- **Embedder session management**: `clear_session()` method releases ~500MB ONNX session memory during idle periods in long-running processes.
- **75 new tests** across search, store, reference, CLI, and MCP modules. Total: 339 lib + 243 integration tests.
- **FTS5 query sanitization**: Special characters and reserved words stripped before MATCH queries, preventing query syntax errors on user input.
- **Cursor-based embedding pagination**: `EmbeddingBatchIterator` uses `WHERE rowid > N` instead of `LIMIT/OFFSET` for stable iteration under concurrent writes.
- **GatherOptions builder API**: Fluent builder methods for configuring gather operations programmatically.
- **Store schema downgrade guard**: `migrate()` returns `StoreError::SchemaNewerThanCq` when index was created by a newer version.
- **WSL path detection**: Permission checks skip chmod on WSL-mounted filesystems where it silently fails.

### Fixed
- **125 audit fixes** from comprehensive 14-category code audit (9 PRs, P1-P3 priorities).
- **Byte truncation panics**: `normalize_for_fts` and notes list use `floor_char_boundary` for safe multi-byte string truncation.
- **Dead code false positives**: Trait impl detection checks parent chunk type instead of method body content.
- **Search fairness**: `BoundedScoreHeap` uses `>=` for equal-score entries, preventing iteration-order bias.
- **Gather determinism**: Tiebreak by name when scores are equal for reproducible results.
- **CLI limit validation**: `--limit` clamped to 1..100 range.
- **Config/project file locking**: Read-modify-write operations use file locks to prevent concurrent corruption.
- **Atomic watch mode updates**: Delete-then-reinsert wrapped in transactions for crash safety.
- **Pipeline transaction safety**: Chunk and call graph inserts in single transaction.
- **HNSW cross-device rename**: Fallback to copy+delete when temp file is on different filesystem.
- **Reference config trust boundary**: Warnings when reference config overrides project settings.
- **Path traversal protection**: `tool_context` validates paths before file access.
- **Protocol version truncation**: HTTP transport truncates version header to prevent abuse.
- **Embedding dimension validation**: `Embedding::new()` validates vector dimensions on construction.
- **Language::def() returns Option**: No more panics on unknown language variants.

### Changed
- **Shared library modules**: Extracted `resolve_target`, focused-read, note injection, impact analysis, and JSON serialization from duplicated CLI/MCP implementations into shared library code.
- **Observability**: 15+ tracing spans added across search, reference, embedder, and store operations. `eprintln` calls migrated to structured `tracing` logging.
- **Error handling**: Silent `.ok()` calls replaced with proper error propagation or degradation warnings.
- **Performance**: Watch mode batch upserts, embedding cache (hash-based skip), `search_by_names_batch` batched FTS, `bytemuck` for embedding serialization, lazy dead code content loading.
- **Dependencies**: `rand` 0.10, `cuvs` 26.2, `colored` 3.1.

## [0.9.7] - 2026-02-08

### Added
- **CLI-first migration**: All cqs features now available via CLI without MCP server. New commands: `cqs notes add/update/remove`, `cqs audit-mode on/off`, `cqs read <path> [--focus fn]`. New search flags: `--name-only`, `--semantic-only`. File-based audit mode persistence (`.cqs/audit-mode.json`) shared between CLI and MCP.
- **Hot-reload reference indexes**: MCP server detects config file changes and reloads reference indexes automatically. No restart needed after `cqs ref add/remove`.

### Fixed
- **Renamed `.cq/` index directory to `.cqs/`** for consistency with binary name, config directory, and config file. Auto-migration renames existing `.cq/` directories on first access. Cross-project search falls back to `.cq/` for unmigrated projects.

## [0.9.6] - 2026-02-08

### Added
- **Markdown language support**: 9th language. Indexes `.md` and `.mdx` files with heading-based chunking, adaptive heading detection (handles both standard and inverted hierarchies), and cross-reference extraction from links and backtick function patterns.
- `ChunkType::Section` for documentation chunks
- `SignatureStyle::Breadcrumb` for heading-path signatures (e.g., "Doc Title > Chapter > Subsection")
- `scripts/clean_md.py` for one-time PDF-to-markdown artifact preprocessing
- `lang-markdown` feature flag (enabled by default)
- Optional `grammar` field on `LanguageDef` for non-tree-sitter languages

## [0.9.5] - 2026-02-08

### Fixed
- **T-SQL name extraction**: `ALTER PROCEDURE` and `ALTER FUNCTION` now indexed (previously only `CREATE` variants)
- **Tree-sitter error recovery**: Position-based validation detects when `@name` capture matched wrong node; falls back to regex extraction from content text
- **Multi-line names**: Truncate to first line when tree-sitter error recovery extends name nodes past actual identifier
- Bump `tree-sitter-sequel-tsql` to 0.4.2 (bracket-quoted identifier support)

## [0.9.4] - 2026-02-07

### Added
- **SQL language support**: 8th language. Parses stored procedures, functions, and views from `.sql` files via forked [tree-sitter-sql](https://github.com/jamie8johnson/tree-sitter-sql) grammar with `CREATE PROCEDURE`, `GO` batch separator, and `EXEC` statement support.
- `SignatureStyle::UntilAs` for SQL's `AS BEGIN...END` pattern
- Schema-qualified name preservation (`dbo.usp_GetOrders`)
- SQL call graph extraction (function invocations + `EXEC`/`EXECUTE` statements)

## [0.9.3] - 2026-02-07

### Fixed
- **Gather search quality**: `gather()` and `search_across_projects()` now use RRF hybrid search instead of raw embedding-only cosine similarity. Previously missed results that keyword matching would find.

### Added
- `cqs_search` `note_only` parameter to search notes exclusively
- `cqs_context` `--summary` mode for condensed file overview
- `cqs_impact` `--format mermaid` output for dependency diagrams

## [0.9.2] - 2026-02-07

### Fixed
- **96 audit fixes** across P1 (43), P2 (23), P3 (30) from 14-category code audit
- **Config safety**: `add_reference_to_config` no longer destroys config on I/O errors
- **Watch mode**: call graph now updates during incremental reindex
- **Gather**: results sorted by score before truncation (was file order)
- **Diff**: language filter uses stored language field instead of file extension matching
- **Search robustness**: limit=0 early return, NaN score defense in BoundedScoreHeap, max_tokens=0 guard
- **Migration safety**: schema migrations wrapped in single transaction
- **Watch paths**: `dunce::canonicalize` for Windows UNC path handling
- **Config validation**: reference weights clamped to [0.0, 1.0], reference count limited to 20
- **Error propagation**: unwrap → Result throughout CLI and MCP tools
- **N+1 queries**: batched embedding lookups in diff and pipeline
- **Code consolidation**: DRY refactors in explain.rs, search.rs, notes.rs

### Added
- Tracing spans on `search_unified` and `search_by_candidates` for performance visibility
- MCP observability: tool entry/exit logging, client info on connect, pipeline stats
- Docstrings for `cosine_similarity` variants and `tool_stats` response fields
- Integration tests: dead code, semantic diff, gather BFS, call graph, reference search, MCP format
- `ChunkIdentity.language` field for language-aware operations
- MCP tool count corrected: 20 (was documented as 21)

### Changed
- `run_migration` accepts `&mut SqliteConnection` instead of `&SqlitePool` for transaction safety
- Context dedup uses typed struct instead of JSON string comparison

## [0.9.1] - 2026-02-06

### Changed
- **Refactor**: Split `parser.rs` (1072 lines) into `src/parser/` directory — mod.rs, types.rs, chunk.rs, calls.rs
- **Refactor**: Split `hnsw.rs` (1150 lines) into `src/hnsw/` directory — mod.rs, build.rs, search.rs, persist.rs, safety.rs
- Updated public-facing messaging to lead with token savings for AI agents
- Enhanced `groom-notes` skill with Phase 2 (suggest new notes from git history)
- Updated CONTRIBUTING.md architecture tree for new directory layout

### Fixed
- Flaky `test_loaded_index_multiple_searches` — replaced sin-based test embeddings with well-separated one-hot vectors

## [0.9.0] - 2026-02-06

### Added
- **`--chunk-type` filter** (CLI + MCP): narrow search to function/method/class/struct/enum/trait/interface/constant
- **`--pattern` filter** (CLI + MCP): post-search structural matching — builder, error_swallow, async, mutex, unsafe, recursion
- **`cqs dead`** (CLI + MCP): find functions/methods never called by indexed code. Excludes main, tests, trait impls. `--include-pub` for full audit
- **`cqs gc`** (CLI + MCP): prune chunks for deleted files, clean orphan call graph entries, rebuild HNSW. MCP reports staleness without modifying
- **`cqs gather`** (CLI + MCP): smart context assembly — BFS call graph expansion from semantic seed results. `--expand`, `--direction`, `--limit` params
- **`cqs project`** (CLI): cross-project search via `~/.config/cqs/projects.toml` registry. `register`, `list`, `remove`, `search` subcommands
- **`--format mermaid`** on `cqs trace`: generate Mermaid diagrams from call paths
- **Index staleness warnings**: `cqs stats` and MCP stats report stale/missing file counts
- 31 new unit tests (structural patterns, gather algorithm, project registry)
- MCP tool count: 17 → 21

## [0.8.0] - 2026-02-07

### Added
- **`cqs trace`** (CLI + MCP): follow a call chain between two functions — BFS shortest path through the call graph with file/line/signature enrichment
- **`cqs impact`** (CLI + MCP): impact analysis — what breaks if you change a function. Returns callers with call-site snippets, transitive callers (with `--depth`), and affected tests via reverse BFS
- **`cqs test-map`** (CLI + MCP): map functions to tests that exercise them — finds tests reachable via reverse call graph traversal with full call chains
- **`cqs batch`** (MCP-only): execute multiple queries in a single tool call — supports search, callers, callees, explain, similar, stats. Max 10 queries per batch
- **`cqs context`** (CLI + MCP): module-level understanding — lists all chunks, external callers/callees, dependent files, and related notes for a given file
- **Focused `cqs_read`**: new `focus` parameter on `cqs_read` MCP tool — returns target function + type dependencies instead of the whole file, cutting tokens by 50-80%
- Store methods: `get_call_graph()`, `get_callers_with_context()`, `find_test_chunks()`, `get_chunks_by_origin()`
- Shared `resolve.rs` modules for CLI and MCP target resolution (deduplicates parse_target/resolve_target from explain/similar)
- `CallGraph` and `CallerWithContext` types in store helpers
- MCP tool count: 12 → 17

## [0.7.0] - 2026-02-06

### Added
- **`cqs similar`** (CLI + MCP): find semantically similar functions by using a stored embedding as the query vector — search by example instead of by text
- **`cqs explain`** (CLI + MCP): generate a function card with signature, docs, callers, callees, and top-3 similar functions in one call
- **`cqs diff`** (CLI + MCP): semantic diff between indexed snapshots — compare project vs reference or two references, reports added/removed/modified with similarity scores
- **Workspace-aware indexing**: detect Cargo workspace root from member crates so `cqs index` indexes the whole workspace
- Store methods: `get_chunk_with_embedding()`, `all_chunk_identities()`, `ChunkIdentity` type

## [0.6.0] - 2026-02-06

### Added
- **Multi-index search**: search across project + reference codebases simultaneously
  - `cqs ref add <name> <source>` — index an external codebase as a reference
  - `cqs ref list` — show configured references with chunk/vector counts
  - `cqs ref remove <name>` — remove a reference and its index files
  - `cqs ref update <name>` — re-index a reference from its source
  - MCP `cqs_search` with `sources` parameter to filter which indexes to search
  - Score-based merge with configurable weight multiplier (default 0.8)
  - `cqs doctor` validates reference index health
  - `[[reference]]` config entries in `.cqs.toml`

### Fixed
- **P1 audit fixes** (12 items): path traversal in glob filter, pipeline mtime race, threshold consistency, SSE origin validation, stale documentation, error message leaks
- **P2 audit fixes** (5 items): dead `search_unified()` removal, CAGRA streaming gap, brute-force note search O(n) elimination, call graph error propagation, config parse error surfacing
- **P3 audit fixes** (11 items): `check_interrupted` stale flag, `unreachable!()` in name_only search, duplicated glob compilation, empty query bypass, CRLF handling, config file permissions (0o600), duplicated note insert SQL, HNSW match duplication, pipeline parse error reporting, panic payload extraction, IO error context in note rewrite

## [0.5.3] - 2026-02-06

### Added
- CJK tokenization: Chinese, Japanese, Korean characters split into individual FTS tokens
- `ChunkRow::from_row()` centralized SQLite row mapping in store layer
- `fetch_chunks_by_ids_async()` and `fetch_chunks_with_embeddings_by_ids_async()` store methods

### Changed
- `tool_add_note` uses `toml::to_string()` via serde instead of manual string escaping
- `search.rs` no longer constructs `ChunkRow` directly from raw SQLite rows

## [0.5.2] - 2026-02-06

### Added
- `cqs stats` now shows note count and call graph summary (total calls, unique callers, unique callees)
- `cqs notes list` CLI command to display all project notes with sentiment
- `cqs_update_note` and `cqs_remove_note` MCP tools for managing notes
- 8 Claude Code skills: audit, bootstrap, docs-review, groom-notes, pr, reindex, release, update-tears

### Changed
- Notes excluded from HNSW/CAGRA index; always brute-force from SQLite for freshness
- 4 safe skills (update-tears, groom-notes, docs-review, reindex) auto-invoke without `/` prefix

### Fixed
- README: documented `cqs_update_note`, `cqs_remove_note` MCP tools
- SECURITY: documented `docs/notes.toml` as MCP write path
- CONTRIBUTING: architecture overview updated with all skills

## [0.5.1] - 2026-02-05

### Fixed
- Algorithm correctness: glob filter applied BEFORE heap in brute-force search (was producing wrong results)
- `note_weight=0` now correctly excludes notes from unified search (was only zeroing scores)
- Windows path extraction in brute-force search uses `origin` column instead of string splitting
- GPU-to-CPU fallback no longer double-windows chunks
- Atomic note replacement (single transaction instead of delete+insert)
- Error propagation: 6 silent error swallowing sites now propagate errors
- Non-finite score validation (NaN/infinity checks in cosine similarity and search filters)
- FTS5 name query: terms now quoted to prevent syntax errors
- Empty query guard for `search_by_name`
- `split_into_windows` returns Result instead of panicking via assert
- Store Drop: `catch_unwind` around `block_on` to prevent panic in async contexts
- Stdio transport: line reads capped at 1MB
- `follow_links(false)` on filesystem walker (prevents symlink loops)
- `.cq/` directory created with 0o700 permissions
- `parse_file_calls` file size guard matching `parse_file`
- HNSW `count_vectors` size guard matching `load()`
- SQL IN clause batching for `get_embeddings_by_hashes` (chunks of 500)
- SQLite cache_size reduced from 64MB to 16MB per connection
- Path normalization gaps fixed in call_graph, graph, stats, filesystem source

### Changed
- `strip_unc_prefix` deduplicated into shared `path_utils` module
- `load_hnsw_index` deduplicated into `HnswIndex::try_load()`
- `index_notes_from_file` deduplicated — CLI now calls `cqs::index_notes()`
- MCP JSON-RPC types restricted to `pub(crate)` visibility
- Regex in `sanitize_error_message` compiled once via `LazyLock`
- `EMBEDDING_DIM` consolidated to single constant in `lib.rs`
- MCP stats uses `count_vectors()` instead of full HNSW load
- `note_stats` returns named struct instead of tuple
- Pipeline call graph upserts batched into single transaction
- HTTP server logging: `eprintln!` replaced with `tracing`
- MCP search: timing span added for observability
- GPU/CPU thread termination now logged
- Error sanitization regex covers `/mnt/` paths
- Watch mode: mtime cached per-file for efficiency
- Batch metadata checks on Store::open (single query)
- Consolidated note_stats and call_stats into fewer queries
- Dead code removed from `cli::run()`
- HNSW save uses streaming checksum (BufReader)
- Model BLAKE3 checksums populated for E5-base-v2

### Added
- 15 new search tests (HNSW-guided, brute-force, glob, language, unified, FTS)
- Test count: 379 (no GPU) up from 364

### Documentation
- `lib.rs` language list updated (C, Java)
- HNSW params corrected (M=24, ef_search=100)
- Cache size corrected (32 not 100)
- Roadmap phase updated
- Chunk cap documented as 100 lines
- Architecture tree updated with CLI/MCP submodules

## [0.5.0] - 2026-02-05

### Added
- **C and Java language support** (#222)
  - tree-sitter-c and tree-sitter-java grammars
  - 7 languages total (Rust, Python, TypeScript, JavaScript, Go, C, Java)
- **Test coverage expansion** (#224)
  - 50 new tests across 6 modules (cagra, index, MCP tools, pipeline, CLI)
  - Total: 375 tests (GPU) / 364 (no GPU)

### Changed
- **Model evaluation complete** (#221)
  - E5-base-v2 confirmed as best option: 100% Recall@5 (50/50 eval queries)
- **Parser/registry consolidation** (#223)
  - parser.rs reduced from 1469 to 1056 lines (28% reduction)
  - Parser re-exports Language, ChunkType from language module

## [0.4.6] - 2026-02-05

### Added
- **Schema migration framework** (#188, #215)
  - Migrations run automatically when opening older indexes
  - Falls back to error if no migration path exists
  - Framework ready for future schema changes
- **CLI integration tests** (#206, #213)
  - 12 end-to-end tests using `assert_cmd`
  - Tests for init, index, search, stats, completions
- **Server transport tests** (#205, #213)
  - 3 tests for stdio transport (initialize, tools/list, invalid JSON)
- **Stress tests** (#207, #213)
  - 5 ignored tests for heavy load scenarios
  - Run with `cargo test --test stress_test -- --ignored`
- **`--api-key-file` option** for secure API key loading (#202, #213)
  - Reads key from file, keeps secret out of process list
  - Uses `zeroize` crate for secure memory wiping

### Changed
- **Lazy grammar loading** (#208, #213)
  - Tree-sitter queries compile on first use, not at startup
  - Reduces startup time by 50-200ms
- **Pipeline resource sharing** (#204, #213)
  - Store shared via `Arc` across pipeline threads
  - Single Tokio runtime instead of 3 separate ones
- Note search warning now logs at WARN level when hitting 1000-note limit (#203, #213)

### Fixed
- **Atomic HNSW writes** (#186, #213)
  - Uses temp directory + rename pattern for crash safety
  - All 4 files written atomically together
- CLI test serialization to prevent HuggingFace Hub lock contention in CI

## [0.4.5] - 2026-02-05

### Added
- **20-category audit complete** - All P1-P4 items addressed (#199, #200, #201, #209)
  - ~243 findings across security, correctness, maintainability, and test coverage
  - Future improvements tracked in issues #202-208

### Changed
- FTS errors now propagate instead of silently failing (#201)
- Note scan capped at 1000 entries for memory safety (#201)
- HNSW build progress logging shows chunk/note breakdown (#201)

### Fixed
- Unicode/emoji handling in FTS5 search (#201)
- Go return type extraction for multiple returns (#201)
- CAGRA batch progress logging (#201)

## [0.4.4] - 2026-02-05

### Added
- **`note_weight` parameter** for controlling note prominence in search results (#183)
  - CLI: `--note-weight 0.5` (0.0-1.0, default 1.0)
  - MCP: `note_weight` parameter in cqs_search
  - Lower values make notes rank below code with similar semantic scores

### Changed
- CAGRA GPU index now uses streaming embeddings and includes notes (#180)
- Removed dead `search_unified()` function (#182) - only `search_unified_with_index()` was used

## [0.4.3] - 2026-02-05

### Added
- **Streaming HNSW build** for large repos (#107)
  - `Store::embedding_batches()` streams embeddings in 10k batches via LIMIT/OFFSET
  - `HnswIndex::build_batched()` builds index incrementally
  - Memory: O(batch_size) instead of O(n) - ~30MB peak instead of ~300MB for 100k chunks
- **Notes in HNSW index** for O(log n) search (#103)
  - Note IDs prefixed with `note:` in unified HNSW index
  - `Store::note_embeddings()` and `search_notes_by_ids()` for indexed note search
  - Index output now shows: `HNSW index: N vectors (X chunks, Y notes)`

### Changed
- HNSW build moved after note indexing to include notes in unified index

### Fixed
- O(n) brute-force note search eliminated - now uses HNSW candidates

## [0.4.2] - 2026-02-05

### Added
- GPU failures counter in index summary output
- `VectorIndex::name()` method for HNSW/CAGRA identification
- `active_index` field in cqs_stats showing which vector index is in use

### Changed
- `Config::merge` renamed to `override_with` for clarity
- `Language::FromStr` now returns `ParserError::UnknownLanguage` (thiserror) instead of anyhow
- `--verbose` flag now sets tracing subscriber to debug level
- Note indexing logic deduplicated into shared `cqs::index_notes()` function

### Fixed
- `check_cq_version` now logs errors at debug level instead of silently discarding
- Doc comments added for `IndexStats`, `UnifiedResult`, `CURRENT_SCHEMA_VERSION`

## [0.4.1] - 2026-02-05

### Changed
- Updated crates.io keywords for discoverability: added `mcp-server`, `vector-search`
- Added GitHub topics: `model-context-protocol`, `ai-coding`, `vector-search`, `onnx`

## [0.4.0] - 2026-02-05

### Added
- **Definition search mode** (`name_only`) for cqs_search (#165)
  - Use `name_only=true` for "where is X defined?" queries
  - Skips semantic embedding, searches function/struct names directly
  - Scoring: exact match 1.0, prefix 0.9, contains 0.7
  - Faster than glob for definition lookups
- `count_vectors()` method for fast HNSW stats without loading full index

### Changed
- CLI refactoring: extracted `watch.rs` from `mod.rs` (274 lines)
  - `cli/mod.rs` reduced from 2167 to 1893 lines

### Fixed
- P2 audit fixes (PRs #161-163):
  - HNSW checksum efficiency (hash from memory, not re-read file)
  - TOML injection prevention in note mentions
  - Memory caps for watch mode and note parsing (10k limits)
  - Platform-specific libc dependency (cfg(unix))

## [0.3.0] - 2026-02-04

### Added
- `cqs_audit_mode` MCP tool for bias-free code reviews (#101)
  - Excludes notes from search/read results during audits
  - Auto-expires after configurable duration (default 30m)
- Error path test coverage (#126, #149)
  - HNSW corruption tests: checksum mismatch, truncation, missing files
  - Schema validation tests: future/old version rejection, model mismatch
  - MCP edge cases: unicode queries, concurrent requests, nested JSON
- Unit tests for embedder.rs and cli.rs (#62, #132)
  - `pad_2d_i64` edge cases (4 tests)
  - `EmbedderError` display formatting (2 tests)
  - `apply_config_defaults` behavior (3 tests)
  - `ExitCode` values (1 test)
- Doc comments for CLI command functions (#70, #137)
- Test helper module `tests/common/mod.rs` (#137)
  - `TestStore` for automatic temp directory setup
  - `test_chunk()` and `mock_embedding()` utilities

### Changed
- Refactored `cmd_serve` to use `ServeConfig` struct (#138)
  - Removes clippy `too_many_arguments` warning
- Removed unused `ExitCode` variants (`IndexMissing`, `ModelMissing`) (#138)
- **Refactored Store module** (#125, #133): Split 1,916-line god object into focused modules
  - `src/store/mod.rs` (468 lines) - Store struct, open/init, FTS5, RRF
  - `src/store/chunks.rs` (352 lines) - Chunk CRUD operations
  - `src/store/notes.rs` (197 lines) - Note CRUD and search
  - `src/store/calls.rs` (220 lines) - Call graph storage/queries
  - `src/store/helpers.rs` (245 lines) - Types, embedding conversion
  - `src/search.rs` (531 lines) - Search algorithms, scoring
  - Largest file reduced from 1,916 to 531 lines (3.6x reduction)

### Fixed
- **CRITICAL**: MCP server concurrency issues (#128)
  - Embedder: `Option<T>` → `OnceLock<T>` for thread-safe lazy init
  - Audit mode: direct field → `Mutex<T>` for safe concurrent access
  - HTTP handler: `write()` → `read()` lock (concurrent reads safe)
- `name_match_score` now preserves camelCase boundaries (#131, #133)
  - Tokenizes before lowercasing instead of after

### Closed Issues
- #62, #70, #101, #102-#114, #121-#126, #142-#146, #148

## [0.2.1] - 2026-02-04

### Added
- Minimum Supported Rust Version (MSRV) declared: 1.88 (required by `ort` dependency)
- `homepage` and `readme` fields in Cargo.toml

### Changed
- Exclude internal files from crate package (AI context, audit docs, dev tooling)

## [0.2.0] - 2026-02-03

### Security
- **CRITICAL**: Fixed timing attack in API key validation using `subtle::ConstantTimeEq`
- Removed `rsa` vulnerability (RUSTSEC-2023-0071) by disabling unused sqlx default features

### Added
- IPv6 localhost support in origin validation (`http://[::1]`, `https://[::1]`)
- Property-based tests (9 total) for RRF fusion, embedder normalization, search bounds
- Fuzz tests (17 total) across nl.rs, note.rs, store.rs, mcp.rs for parser robustness
- MCP protocol edge case tests (malformed JSON-RPC, oversized payloads, unicode)
- FTS5 special character tests (wildcards, quotes, colons)
- Expanded SECURITY.md with threat model, trust boundaries, attack surface documentation
- Discrete sentiment scale documentation in CLAUDE.md

### Changed
- Split cli.rs into cli/ module (mod.rs + display.rs) for maintainability
- Test count: 75 → 162 (2x+ increase)
- `proptest` added to dev-dependencies

### Fixed
- RRF score bound calculation (duplicates can boost scores above naive maximum)
- `unwrap()` → `expect()` with descriptive messages (10 locations)
- CAGRA initialization returns empty vec instead of panic on failure
- Symlink logging in embedder (warns instead of silently skipping)
- clamp fix in `get_chunk_by_id` for edge cases

### Closed Issues
- #64, #66, #67, #68, #69, #74, #75, #76, #77, #78, #79, #80, #81, #82, #83, #84, #85, #86

## [0.1.18] - 2026-02-03

### Added
- `--api-key` flag and `CQS_API_KEY` env var for HTTP transport authentication
  - Required for non-localhost network exposure
  - Constant-time comparison to prevent timing attacks
- `--bind` flag to specify listen address (default: 127.0.0.1)
  - Non-localhost binding requires `--dangerously-allow-network-bind` and `--api-key`

### Changed
- Migrated from rusqlite to sqlx async SQLite (schema v10)
- Extracted validation functions for better code discoverability
  - `validate_api_key`, `validate_origin_header`, `validate_query_length`
  - `verify_hnsw_checksums` with extension allowlist
- Replaced `unwrap()` with `expect()` for better panic messages
- Added SAFETY comments to all unsafe blocks

### Fixed
- Path traversal vulnerability in HNSW checksum verification
- Integer overflow in saturating i64→u32 casts for database fields

### Security
- Updated `bytes` to 1.11.1 (RUSTSEC-2026-0007 integer overflow fix)
- HNSW checksum verification now validates extensions against allowlist

## [0.1.17] - 2026-02-01

### Added
- `--gpu` flag for `cqs serve` to enable GPU-accelerated query embedding
  - CPU (default): cold 0.52s, warm 22ms
  - GPU: cold 1.15s, warm 12ms (~45% faster warm queries)

### Changed
- Hybrid CAGRA/HNSW startup: HNSW loads instantly (~30ms), CAGRA builds in background
  - Server ready immediately, upgrades to GPU index transparently
  - Eliminates 1.2s blocking startup delay

### Fixed
- Search results now prioritize code over notes (60/40 split)
  - Notes enhance but don't dominate results
  - Reserve 60% of slots for code, notes fill the rest

## [0.1.16] - 2026-02-01

### Added
- Tracing spans for major operations (`cmd_index`, `cmd_query`, `embed_batch`, `search_filtered`)
- Version check warning when index was created by different cqs version
- `Embedding` type encapsulation with `as_slice()`, `as_vec()`, `len()` methods

### Fixed
- README: Corrected call graph documentation (cross-file works, not within-file only)
- Bug report template: Updated version placeholder

### Documentation
- Added security doc comment for MCP origin validation behavior

## [0.1.15] - 2026-02-01

### Added
- Full call graph coverage for large functions (>100 lines)
  - Separate `function_calls` table captures all calls regardless of chunk size limits
  - CLI handlers like `cmd_index` now have call graph entries
  - 1889 calls captured vs ~200 previously

### Changed
- Schema version: 4 → 5 (requires `cqs index --force` to rebuild)

## [0.1.14] - 2026-01-31

### Added
- Call graph analysis (`cqs callers`, `cqs callees`)
  - Extract function call relationships from source code
  - Find what calls a function and what a function calls
  - MCP tools: `cqs_callers`, `cqs_callees`
  - tree-sitter queries for call extraction across all 5 languages

### Changed
- Schema version: 3 → 4 (adds `calls` table)

## [0.1.13] - 2026-01-31

### Added
- NL module extraction (src/nl.rs)
  - `generate_nl_description()` for code→NL→embed pipeline
  - `tokenize_identifier()` for camelCase/snake_case splitting
  - JSDoc parsing for JavaScript (@param, @returns tags)
- Eval improvements
  - Eval suite uses NL pipeline (matches production)
  - Runs in CI on tagged releases

## [0.1.12] - 2026-01-31

### Added
- Code→NL embedding pipeline (Greptile approach)
  - Embeds natural language descriptions instead of raw code
  - Generates: "A function named X. Takes parameters Y. Returns Z."
  - Doc comments prioritized as human-written NL
  - Identifier normalization: `parseConfig` → "parse config"

### Changed
- Schema version: 2 → 3 (requires `cqs index --force` to rebuild)

### Breaking Changes
- Existing indexes must be rebuilt with `--force`

## [0.1.11] - 2026-01-31

### Added
- MCP: `semantic_only` parameter to disable RRF hybrid search when needed
- MCP: HNSW index status in `cqs_stats` output

### Changed
- tree-sitter-rust: 0.23 -> 0.24
- tree-sitter-python: 0.23 -> 0.25
- Raised brute-force warning threshold from 50k to 100k chunks

### Documentation
- Simplified CLAUDE.md and tears system
- Added docs/SCARS.md for failed approaches
- Consolidated PROJECT_CONTINUITY.md (removed dated files)

## [0.1.10] - 2026-01-31

### Added
- RRF (Reciprocal Rank Fusion) hybrid search combining semantic + FTS5 keyword search
- FTS5 virtual table for full-text keyword search
- `normalize_for_fts()` for splitting camelCase/snake_case identifiers into searchable words
- Chunk-level incremental indexing (skip re-embedding unchanged chunks via content_hash)
- `Store::get_embeddings_by_hashes()` for batch embedding lookup

### Changed
- Schema version bumped from 1 to 2 (FTS5 support)
- RRF enabled by default in CLI and MCP for improved recall

## [0.1.9] - 2026-01-31

### Added
- HNSW-guided filtered search (10-100x faster for filtered queries)
- SIMD-accelerated cosine similarity via simsimd crate
- Shell completion generation (`cqs completions bash/zsh/fish/powershell`)
- Config file support (`.cqs.toml` in project, `~/.config/cqs/config.toml` for user)
- Lock file with PID for stale lock detection
- Rustdoc documentation for public API

### Changed
- Error messages now include actionable hints
- Improved unknown language/tool error messages

## [0.1.8] - 2026-01-31

### Added
- HNSW index for O(log n) search on large codebases (>50k chunks)
- Automatic HNSW index build after indexing
- Query embedding LRU cache (32 entries)

### Fixed
- RwLock poison recovery in HTTP handler
- LRU cache poison recovery in embedder
- Query length validation (8KB max)
- Embedding byte validation with warning

## [0.1.7] - 2026-01-31

### Fixed
- Removed `Parser::default()` panic risk
- Added logging for silent search errors
- Clarified embedder unwrap with expect()
- Added parse error logging in watch mode
- Added 100KB chunk byte limit (handles minified files)
- Graceful HTTP shutdown with Ctrl+C handler
- Protocol version constant consistency

## [0.1.6] - 2026-01-31

### Added
- Connection pooling with r2d2-sqlite (4 max connections)
- Request body limit (1MB) via tower middleware
- Secure UUID generation (timestamp + random)

### Fixed
- lru crate vulnerability (0.12 -> 0.16, GHSA-rhfx-m35p-ff5j)

### Changed
- Store methods now take `&self` instead of `&mut self`

## [0.1.5] - 2026-01-31

### Added
- SSE stream support via GET /mcp
- GitHub Actions CI workflow (build, test, clippy, fmt)
- Issue templates for bug reports and feature requests
- GitHub releases with changelogs

## [0.1.4] - 2026-01-31

### Changed
- MCP 2025-11-25 compliance (Origin validation, Protocol-Version header)
- Batching removed per MCP spec update

## [0.1.3] - 2026-01-31

### Added
- Watch mode (`cqs watch`) with debounce
- HTTP transport (MCP Streamable HTTP spec)
- .gitignore support via ignore crate

### Changed
- CLI restructured (query as positional arg, flags work anywhere)
- Replaced walkdir with ignore crate

### Fixed
- Compiler warnings

## [0.1.2] - 2026-01-31

### Added
- New chunk types: Class, Struct, Enum, Trait, Interface, Constant
- Hybrid search with `--name-boost` flag
- Context display with `-C N` flag
- Doc comments included in embeddings

## [0.1.1] - 2026-01-31

### Fixed
- Path pattern filtering (relative paths)
- Invalid language error handling

## [0.1.0] - 2026-01-31

### Added
- Initial release
- Semantic code search for 5 languages (Rust, Python, TypeScript, JavaScript, Go)
- tree-sitter parsing for function/method extraction
- nomic-embed-text-v1.5 embeddings (768-dim) [later changed to E5-base-v2 in v0.1.16]
- GPU acceleration (CUDA/TensorRT) with CPU fallback
- SQLite storage with WAL mode
- MCP server (stdio transport)
- CLI commands: init, doctor, index, stats, serve
- Filter by language (`-l`) and path pattern (`-p`)

[Unreleased]: https://github.com/jamie8johnson/cqs/compare/v0.19.0...HEAD
[0.19.0]: https://github.com/jamie8johnson/cqs/compare/v0.18.0...v0.19.0
[0.18.0]: https://github.com/jamie8johnson/cqs/compare/v0.17.0...v0.18.0
[0.17.0]: https://github.com/jamie8johnson/cqs/compare/v0.16.0...v0.17.0
[0.16.0]: https://github.com/jamie8johnson/cqs/compare/v0.15.0...v0.16.0
[0.15.0]: https://github.com/jamie8johnson/cqs/compare/v0.14.1...v0.15.0
[0.14.1]: https://github.com/jamie8johnson/cqs/compare/v0.14.0...v0.14.1
[0.14.0]: https://github.com/jamie8johnson/cqs/compare/v0.13.1...v0.14.0
[0.13.1]: https://github.com/jamie8johnson/cqs/compare/v0.13.0...v0.13.1
[0.13.0]: https://github.com/jamie8johnson/cqs/compare/v0.12.12...v0.13.0
[0.12.12]: https://github.com/jamie8johnson/cqs/compare/v0.12.11...v0.12.12
[0.12.11]: https://github.com/jamie8johnson/cqs/compare/v0.12.10...v0.12.11
[0.12.10]: https://github.com/jamie8johnson/cqs/compare/v0.12.9...v0.12.10
[0.12.9]: https://github.com/jamie8johnson/cqs/compare/v0.12.8...v0.12.9
[0.12.8]: https://github.com/jamie8johnson/cqs/compare/v0.12.7...v0.12.8
[0.12.7]: https://github.com/jamie8johnson/cqs/compare/v0.12.6...v0.12.7
[0.12.6]: https://github.com/jamie8johnson/cqs/compare/v0.12.5...v0.12.6
[0.12.5]: https://github.com/jamie8johnson/cqs/compare/v0.12.4...v0.12.5
[0.12.4]: https://github.com/jamie8johnson/cqs/compare/v0.12.3...v0.12.4
[0.12.3]: https://github.com/jamie8johnson/cqs/compare/v0.12.2...v0.12.3
[0.12.2]: https://github.com/jamie8johnson/cqs/compare/v0.12.1...v0.12.2
[0.12.1]: https://github.com/jamie8johnson/cqs/compare/v0.12.0...v0.12.1
[0.12.0]: https://github.com/jamie8johnson/cqs/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/jamie8johnson/cqs/compare/v0.10.2...v0.11.0
[0.10.2]: https://github.com/jamie8johnson/cqs/compare/v0.10.1...v0.10.2
[0.10.1]: https://github.com/jamie8johnson/cqs/compare/v0.10.0...v0.10.1
[0.10.0]: https://github.com/jamie8johnson/cqs/compare/v0.9.9...v0.10.0
[0.9.9]: https://github.com/jamie8johnson/cqs/compare/v0.9.8...v0.9.9
[0.9.8]: https://github.com/jamie8johnson/cqs/compare/v0.9.7...v0.9.8
[0.9.7]: https://github.com/jamie8johnson/cqs/compare/v0.9.6...v0.9.7
[0.9.6]: https://github.com/jamie8johnson/cqs/compare/v0.9.5...v0.9.6
[0.9.5]: https://github.com/jamie8johnson/cqs/compare/v0.9.4...v0.9.5
[0.9.4]: https://github.com/jamie8johnson/cqs/compare/v0.9.3...v0.9.4
[0.9.3]: https://github.com/jamie8johnson/cqs/compare/v0.9.2...v0.9.3
[0.9.2]: https://github.com/jamie8johnson/cqs/compare/v0.9.1...v0.9.2
[0.9.1]: https://github.com/jamie8johnson/cqs/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/jamie8johnson/cqs/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/jamie8johnson/cqs/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/jamie8johnson/cqs/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/jamie8johnson/cqs/compare/v0.5.3...v0.6.0
[0.5.3]: https://github.com/jamie8johnson/cqs/compare/v0.5.2...v0.5.3
[0.5.2]: https://github.com/jamie8johnson/cqs/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/jamie8johnson/cqs/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/jamie8johnson/cqs/compare/v0.4.6...v0.5.0
[0.4.6]: https://github.com/jamie8johnson/cqs/compare/v0.4.5...v0.4.6
[0.4.5]: https://github.com/jamie8johnson/cqs/compare/v0.4.4...v0.4.5
[0.4.4]: https://github.com/jamie8johnson/cqs/compare/v0.4.3...v0.4.4
[0.4.3]: https://github.com/jamie8johnson/cqs/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/jamie8johnson/cqs/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/jamie8johnson/cqs/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/jamie8johnson/cqs/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/jamie8johnson/cqs/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/jamie8johnson/cqs/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/jamie8johnson/cqs/compare/v0.1.18...v0.2.0
[0.1.18]: https://github.com/jamie8johnson/cqs/compare/v0.1.17...v0.1.18
[0.1.17]: https://github.com/jamie8johnson/cqs/compare/v0.1.16...v0.1.17
[0.1.16]: https://github.com/jamie8johnson/cqs/compare/v0.1.15...v0.1.16
[0.1.15]: https://github.com/jamie8johnson/cqs/compare/v0.1.14...v0.1.15
[0.1.14]: https://github.com/jamie8johnson/cqs/compare/v0.1.13...v0.1.14
[0.1.13]: https://github.com/jamie8johnson/cqs/compare/v0.1.12...v0.1.13
[0.1.12]: https://github.com/jamie8johnson/cqs/compare/v0.1.11...v0.1.12
[0.1.11]: https://github.com/jamie8johnson/cqs/compare/v0.1.10...v0.1.11
[0.1.10]: https://github.com/jamie8johnson/cqs/compare/v0.1.9...v0.1.10
[0.1.9]: https://github.com/jamie8johnson/cqs/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/jamie8johnson/cqs/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/jamie8johnson/cqs/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/jamie8johnson/cqs/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/jamie8johnson/cqs/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/jamie8johnson/cqs/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/jamie8johnson/cqs/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/jamie8johnson/cqs/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/jamie8johnson/cqs/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/jamie8johnson/cqs/releases/tag/v0.1.0
