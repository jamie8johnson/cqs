# Audit Triage — post-v1.27.0 (2026-04-18 → 2026-04-19, complete)

150 findings across 16 categories. Source: `docs/audit-findings.md`.

**Status: complete.** All P1-P3 landed in PRs #1041 / #1045 / #1046; 3 hard P4s filed as issues #1042 / #1043 / #1044; 3 trivial P4s deferred or obviated.

Severity buckets:
- **P1**: easy + high impact — fix immediately
- **P2**: medium effort + high impact — fix in batch
- **P3**: easy + low impact — fix if time
- **P4**: hard or low impact — defer / inline trivials / open issues

Status column: `pending` → `in flight` → `✅ PR #N` (or `✅ inline` / `✅ filed: #N`).

## P1 — easy + high impact (fix immediately)

| # | Title | Location | Status |
|---|---|---|---|
| 1 | `emit_json` (CLI) skips NaN/Infinity sanitization that `write_json_line` (batch) performs — silent panic / lost output | `src/cli/json_envelope.rs:121-125` + `src/cli/batch/mod.rs:1061-1087` | ✅ PR #1041 |
| 2 | `cmd_chat` REPL JSON formatting does not sanitize NaN/Infinity — same defect as #1, third surface | `src/cli/chat.rs:225-231` | ✅ PR #1041 |
| 3 | `extract_doc_fallback_for_short_chunk` treats `#[derive]`, `#include`, `#define`, `*ptr` as comment-like — leaks attribute/preprocessor/code into doc | `src/parser/chunk.rs:264-276` | ✅ PR #1041 |
| 4 | Walk-back loop counts blank lines toward `FALLBACK_DOC_MAX_LINES` then strips them — real comments past blank gaps silently truncated | `src/parser/chunk.rs:311-327` | ✅ PR #1041 |
| 5 | `BoundedScoreHeap::push` evicts the *best* tied-score id instead of the *worst* — non-deterministic top-K under HashMap-fed input | `src/search/scoring/candidate.rs:193-217` | ✅ PR #1041 |
| 6 | `dot()` for neighbor search silently truncates on dim mismatch — wrong scores after partial reindex | `src/cli/commands/search/neighbors.rs:67-70` | ✅ PR #1041 |
| 7 | `cqs --json eval queries.json` does not emit JSON — top-level `--json` cascade missing | `src/cli/dispatch.rs:495` + `src/cli/commands/eval/mod.rs:95-99` | ✅ PR #1041 |
| 8 | `cqs --json project search` and `cqs --json cache stats` ignore top-level `--json` | `src/cli/dispatch.rs:69,147-149` | ✅ PR #1041 |
| 9 | `cqs ping --json` (and `cqs eval --baseline`) emit text on no-daemon error path — JSON consumers get nothing on stdout | `src/cli/commands/infra/ping.rs:182-188`, `src/cli/commands/eval/mod.rs:117-122` | ✅ PR #1041 |
| 10 | `notes add --sentiment NaN` poisons notes.toml — missing `parse_finite_f32` value_parser | `src/cli/commands/io/notes.rs:69-70,86-87` + `src/cli/commands/infra/reference.rs:48-50` | ✅ PR #1041 |
| 11 | `cqs cache stats --json` returns `total_size_mb` as a string, breaks numeric consumers | `src/cli/commands/infra/cache_cmd.rs:54-61` | ✅ PR #1041 |
| 12 | `QueryCache::get` byte-slice query preview panics on multi-byte chars near offset 40 | `src/cache.rs:1021-1028` | ✅ PR #1041 |
| 13 | `run_git_log` truncates git stderr by raw byte position — panics on non-ASCII paths | `src/cli/commands/io/blame.rs:144-152` | ✅ PR #1041 |
| 14 | `dispatch_line` (CLI batch) missing the NUL-byte check the daemon socket loop enforces — divergent input validation | `src/cli/batch/mod.rs:466-510` vs `:1329-1343` | ✅ PR #1041 |
| 15 | `dispatch_line` and `cmd_batch` envelope errors emit no `tracing::warn!` — daemon fails silently in journal | `src/cli/batch/mod.rs:466-510, 1306-1401` | ✅ PR #1041 |
| 16 | Migration `UPDATE schema_version` silently does nothing if metadata row is missing — re-run failure | `src/store/migrations.rs:191-194` | ✅ PR #1041 |
| 17 | `function_calls` rows leaked on every incremental delete (`prune_missing`, `delete_by_origin`, `delete_phantom_chunks`) — ghost callers | `src/store/chunks/staleness.rs`, `src/store/chunks/crud.rs:427-449,539-605` | ✅ PR #1041 |
| 18 | `EmbeddingCache::open` silently swallows `set_permissions(0o600)` errors — asymmetric with QueryCache after SEC-V1.25-4 | `src/cache.rs:131-147` | ✅ PR #1041 |
| 19 | Path-traversal absolute-path check `as_bytes()[1] == b':'` doesn't detect Windows UNC / `\\?\` paths | `src/cli/display.rs:27` | ✅ PR #1041 |
| 20 | `cqs read` `bail!("File not found")` runs before traversal validation — daemon path-existence oracle | `src/cli/commands/io/read.rs:24-29` | ✅ PR #1041 |
| 21 | Daemon socket bind-then-chmod TOCTOU — socket world-creatable for ~ms on `/tmp` fallback | `src/cli/watch.rs:1206-1220` | ✅ PR #1041 |
| 22 | `aux_model::is_path_like` rejects Windows `C:\Models\splade` as HF repo id | `src/aux_model.rs:117-119` | ✅ PR #1041 |
| 23 | `find_python` error message hardcodes Linux/macOS install instructions, never Windows | `src/convert/mod.rs:77-80` | ✅ PR #1041 |
| 24 | `cli/dispatch.rs` notes `Option<CommandContext>` collapses store-open failure into clueless "Index not found" | `src/cli/dispatch.rs:197-200` | ✅ PR #1041 |
| 25 | `dispatch_stats` (daemon) drops staleness fields the CLI populates — silent inconsistency on agent default path | `src/cli/batch/handlers/info.rs:246-252` vs `src/cli/commands/index/stats.rs:283-298` | ✅ PR #1041 |
| 26 | `cqs eval` JSON: `r1`/`r5`/`r20` field names break the `r_at_K` convention used everywhere else | `src/cli/commands/eval/baseline.rs:28-33` | ✅ PR #1041 |

## P2 — medium effort + high impact (batch fix)

| # | Title | Location | Status |
|---|---|---|---|
| 27 | `cqs doctor --json` always prints text checks before JSON — output unparseable | `src/cli/commands/infra/doctor.rs:97-322` | ✅ PR #1045 |
| 28 | `wrap_value(value.clone())` clones entire `serde_json::Value` per daemon record — multi-MB allocator churn | `src/cli/batch/mod.rs:1065` + `src/cli/json_envelope.rs:102-108` | ✅ PR #1045 |
| 29 | PR #1040 short-chunk doc enrichment silently skipped on incremental reindex (no content_hash bump) | `src/parser/chunk.rs:88,105` + `src/store/chunks/async_helpers.rs:261-393` | ✅ PR #1045 |
| 30 | HNSW save: backup `std::fs::rename` failure logged-and-continued — rollback path can lose original | `src/hnsw/persist.rs:407-419` + `:457-479` | ✅ PR #1045 |
| 31 | `acquire_index_lock` stale-lock removal races with peer holding the same lock-file inode — two writers | `src/cli/files.rs:140-179` | ✅ PR #1045 |
| 32 | `prune_all` Phase 1 reads outside the write transaction — TOCTOU vs concurrent watch reindex | `src/store/chunks/staleness.rs:161-275` | ✅ PR #1045 |
| 33 | Daemon batch error envelope `format!("{e:#}")` propagates raw HTTP body / paths to clients with no allowlist | `src/cli/batch/mod.rs:502,507,1380,1393`, `src/llm/batch.rs:69-70,144,174,229-232` | ✅ PR #1045 |
| 34 | `sanitize_untrusted` does not neutralize triple-backticks inside user content — markdown sandbox escape via reference index → `--improve-docs` | `src/llm/prompts.rs:21-55` | ✅ PR #1045 |
| 35 | `convert::is_safe_executable_path` only blocks `/tmp/` `/var/tmp/` — Windows `%TEMP%\python.exe` passes the gate | `src/convert/mod.rs:142-165` | ✅ PR #1045 |
| 36 | `find_pdf_script` falls back to `scripts/pdf_to_md.py` from CWD without ownership / writability check | `src/convert/pdf.rs:81-93` | ✅ PR #1045 |
| 37 | `convert/chm.rs` zip-slip check silently skips entries that fail to canonicalize — broken-symlink extraction can write outside temp_dir | `src/convert/chm.rs:60-87` | ✅ PR #1045 |
| 38 | Three different "dry-run vs apply" idioms across mutating commands — agent can't predict | `src/cli/definitions.rs:296,619`, `src/cli/args.rs:435,544` | ✅ PR #1045 |
| 39 | Pre-existing API-2 / API-3 / API-4 / API-6 / API-13 still open in source — never landed | multiple (see finding) | ✅ PR #1045 |
| 40 | `wrap_value`/`wrap_error` (json! macro) duplicate `Envelope::ok`/`Envelope::err` (typed) — two impls of one shape | `src/cli/json_envelope.rs:71-117` | ✅ PR #1045 |
| 41 | `handle_socket_client` 5s read / 30s write hardcoded — wave-1A TODO leftover; daemon ignores `CQS_DAEMON_TIMEOUT_MS` | `src/cli/watch.rs:153,159` + `src/cli/dispatch.rs:634-636` | ✅ PR #1045 |
| 42 | `Store::open_readonly_pooled{,_with_runtime}` duplicate `StoreOpenConfig` literal — drift risk | `src/store/mod.rs:694-748` | ✅ PR #1045 |
| 43 | `extract_doc_fallback_for_short_chunk` allocates `Vec<&str>` over entire prefix per short chunk — O(N²) parse-time | `src/parser/chunk.rs:289-322` | ✅ PR #1045 |
| 44 | `cmd_blame` (CLI) has zero integration tests — entire `cqs blame` surface untested | `src/cli/commands/io/blame.rs:288-309` | ✅ PR #1045 |
| 45 | `cmd_review` (CLI) has zero integration tests — token-budget surface untested | `src/cli/commands/review/diff_review.rs:8-62` | ✅ PR #1045 |
| 46 | `cmd_plan`/`cmd_task`/`cmd_affected`/`cmd_ci` have zero CLI integration tests | `src/cli/commands/{train,review}/...` | ✅ PR #1045 |
| 47 | `cmd_chat` REPL has zero tests; NaN path silently drops user results (overlaps #2) | `src/cli/chat.rs:134` | ✅ PR #1045 |
| 48 | Batch handlers `dispatch_review`/`dispatch_diff`/`dispatch_drift`/`dispatch_blame`/`dispatch_plan` zero integration tests | `src/cli/batch/handlers/*` | ✅ PR #1045 |
| 49 | `parse_unified_diff` u32 overflow on huge hunk start/count silently defaults to 1 | `src/diff_parse.rs:73-97` | ✅ PR #1045 |
| 50 | `parse_unified_diff` no test for non-`b/`-prefixed `+++` paths — fallback path stores raw | `src/diff_parse.rs:55-60` | ✅ PR #1045 |
| 51 | `dispatch_line` shell_words tokenization not tested for embedded NUL bytes / control chars; `args_preview` echoes control chars to journal | `src/cli/batch/mod.rs:472`, `src/cli/watch.rs:246-262` | ✅ PR #1045 |
| 52 | `extract_doc_fallback_for_short_chunk` lacks tests for `FALLBACK_DOC_MAX_LINES` cap, exact-5-line boundary, mid-UTF-8 start_byte | `src/parser/chunk.rs:289-341` (multiple gaps) | ✅ PR #1045 |
| 53 | `line_looks_comment_like` hardcodes comment prefixes globally — adding a language with new comment syntax silently fails | `src/parser/chunk.rs:264-276` | ✅ PR #1045 |
| 54 | `wrap_error` / `emit_json_error` accept arbitrary `&str` codes — `error_codes` taxonomy isn't compile-enforced | `src/cli/json_envelope.rs:42-130` | ✅ PR #1045 |
| 55 | `parser/markdown::normalize_lang` duplicates the language registry — adding a language won't register fenced aliases | `src/parser/markdown/code_blocks.rs:20-79` | ✅ PR #1045 |
| 56 | Three independent `QueryCategory` definitions across production / tests / evals — drift risk | `tests/eval_common.rs`, `evals/schema.rs`, `src/search/router.rs` | ✅ PR #1045 |
| 57 | `extract_method_name_from_line` has `_ => generic` fallthrough — new languages with `proc`/`procedure`/`sub` silently miss | `src/nl/fields.rs:204-258` | ✅ PR #1045 |
| 58 | `ChunkType::is_callable` and `is_code` use non-exhaustive `matches!()` — adding variant compiles silently | `src/language/mod.rs:692-729` | ✅ PR #1045 |
| 59 | Hardcoded BERT defaults `DEFAULT_MODEL_REPO` / `DEFAULT_DIM` fan out — same pattern as v0.9.0 disaster | `src/embedder/models.rs:139-145, 526-537` | ✅ PR #1045 |
| 60 | `where_to_add::pattern_def_for` `_ => None` arm — adding a language compiles fine but emits no patterns | `src/where_to_add.rs:586-612` | ✅ PR #1045 |
| 61 | `tests/eval_common.rs` / `evals/schema.rs` redundant eval-row types — three sources of truth | multiple | ✅ PR #1045 |
| 62 | Daemon socket response triple-handles bytes: dispatch → UTF-8 validate → re-wrap as JSON-string-in-JSON | `src/cli/watch.rs:281-301` | ✅ PR #1045 |
| 63 | Reindex pipeline re-parses every chunk to extract call edges already in `parse_file_all` Pass 2 | `src/cli/pipeline/parsing.rs:95-100` | ✅ PR #1045 |
| 64 | `upsert_function_calls` opens a separate write transaction per file in indexing inner loop | `src/cli/pipeline/upsert.rs:152-163` | ✅ PR #1045 |
| 65 | `Store::get_type_users`/`get_types_used_by` fetch all rows then truncate at the CLI — should `LIMIT` at SQL | `src/store/types.rs:284-335` | ✅ PR #1045 |
| 66 | `parse_markdown_references` allocates per-section `String` via `lines[..].join("\n")` — O(N×M) | `src/parser/markdown/mod.rs:243-275, 600` | ✅ PR #1045 |
| 67 | `BatchContext::sweep_idle_sessions` clears ONNX but leaves `hnsw`/`splade_index`/graphs resident — daemon idle footprint stays high | `src/cli/batch/mod.rs:266-298` | ✅ PR #1045 |
| 68 | `BatchContext::call_graph` / `test_chunks` Arc caches grow to 100s of MB on large corpora — no eviction except on index change | `src/cli/batch/mod.rs:203-204, 915-947` | ✅ PR #1045 |
| 69 | `BatchContext::config` / `audit_state` `OnceLock`s never re-read — config edits and 30-min audit-mode expire ignored until daemon restart | `src/cli/batch/mod.rs:196-199, 867-953` | ✅ PR #1045 |
| 70 | `EmbeddingCache` / `QueryCache` SqlitePool never explicitly closed — no `Drop` checkpoint, WAL grows unbounded | `src/cache.rs:42-65, 893-899` | ✅ PR #1045 |
| 71 | `cli/commands::infra::model::stop_daemon_best_effort` returns false on macOS — model swap proceeds against live daemon | `src/cli/commands/infra/model.rs:470-505` | ✅ PR #1045 |
| 72 | `cli::commands::io::notes::cmd_notes_add` reports `index_error` only in JSON mode — text users see "Added" with silent reindex failure | `src/cli/commands/io/notes.rs:288-301,400,472` | ✅ PR #1045 |
| 73 | `cli::commands::infra::model::cmd_model_swap` `chunks_indexed = 0` collapses three failure cases | `src/cli/commands/infra/model.rs:336-344` | ✅ PR #1045 |

## P3 — easy + low impact (fix if time)

| # | Title | Location | Status |
|---|---|---|---|
| 74 | README "How It Works" still cites v1.25.0 V2 numbers — contradicts TL;DR | `README.md:610,636-659` | ✅ PR #1046 |
| 75 | `/audit` skill says "14-category" but actually has 16 | `.claude/skills/audit/SKILL.md:10`, `CLAUDE.md:48`, `CONTRIBUTING.md:306` | ✅ PR #1046 |
| 76 | `troubleshoot` skill references nonexistent `cqs serve --stdio` and `CQS_API_KEY` | `.claude/skills/troubleshoot/SKILL.md:60-68` | ✅ PR #1046 |
| 77 | `troubleshoot` skill points at nonexistent `src/store/helpers.rs` (now a directory) | `.claude/skills/troubleshoot/SKILL.md:49` | ✅ PR #1046 |
| 78 | `extract_doc_fallback_for_short_chunk` doc says "<5 lines" but matches ≤5-line | `src/parser/chunk.rs:278,295` | ✅ PR #1046 |
| 79 | `ChunkType::Class` doc lists 3 languages — actually captured by 20 | `src/language/mod.rs:620,628,640` | ✅ PR #1046 |
| 80 | `docs/plans/2026-04-12-persistent-daemon.md` marked Status: Design — daemon shipped | `docs/plans/2026-04-12-persistent-daemon.md:5` | ✅ PR #1046 |
| 81 | CONTRIBUTING.md JSON envelope error_codes doesn't disclose `not_found`/`io_error` are unwired | `CONTRIBUTING.md:79-84` | ✅ PR #1046 |
| 82 | CLAUDE.md "Project Conventions" omits MSRV 1.95 | `CLAUDE.md:213` | ✅ PR #1046 |
| 83 | `LlmClient::sanitize_untrusted` `expect("valid UTF-8")` after byte-step — invariant fragile | `src/llm/prompts.rs:21-55` | ✅ PR #1046 |
| 84 | `extract_doc_fallback_for_short_chunk` slice no boundary diagnostic | `src/parser/chunk.rs:304-306` | ✅ PR #1046 |
| 85 | `parser/markdown::parse_markdown_references` byte slice without boundary check | `src/parser/markdown/mod.rs:594-596` | ✅ PR #1046 |
| 86 | Daemon socket request silently drops non-string args/command | `src/cli/watch.rs:209-217` | ✅ PR #1046 |
| 87 | `build_stats_output` `as u32` cast on schema_version may wrap | `src/cli/commands/index/stats.rs:173` | ✅ PR #1046 |
| 88 | `tolerated_blanks < 4` magic constant in `extract_doc_comment` — sibling of named consts | `src/parser/chunk.rs:193,213` | ✅ PR #1046 |
| 89 | `resolve_splade_alpha` global env arm silently swallows malformed/non-finite values | `src/search/router.rs:449-464` | ✅ PR #1046 |
| 90 | `--verbose` has three different meanings across CLI | `src/cli/definitions.rs:261,302,660` | ✅ PR #1046 |
| 91 | `cmd_doctor --fix` `tracing::warn!` interpolates ExitStatus — unstructured | `src/cli/commands/infra/doctor.rs:59,75` | ✅ PR #1046 |
| 92 | `cmd_notes_add/update/remove` emit identical "Note operation warning" with no op-discriminator | `src/cli/commands/io/notes.rs:299,400,472` | ✅ PR #1046 |
| 93 | `extract_calls`: parse-failure warnings carry no path or chunk identity | `src/parser/calls.rs:15-59` | ✅ PR #1046 |
| 94 | `centroid file contained 0 valid centroids` — no path field | `src/search/router.rs:978` | ✅ PR #1046 |
| 95 | `cli/pipeline/parsing.rs:118` unstructured `tracing::warn!` mid-rayon-reduce | `src/cli/pipeline/parsing.rs:118`, `src/cli/pipeline/mod.rs:143` | ✅ PR #1046 |
| 96 | `extract_doc_fallback_for_short_chunk` `tracing::debug!` lacks chunk file path | `src/parser/chunk.rs:298-340` | ✅ PR #1046 |
| 97 | `notes_path` parse warn fires on absent-file case — should be debug | `src/cli/batch/mod.rs:886` | ✅ PR #1046 |
| 98 | `tracing::info!("Index returned ... candidates")` unstructured emission in hot search path | `src/search/query.rs:764,768` | ✅ PR #1046 |
| 99 | `daemon_ping` emits no spans/tracing on its own error paths | `src/daemon_translate.rs:217-280` | ✅ PR #1046 |
| 100 | Reranker over-retrieval pool `(limit*4).min(100)` duplicated 4x with no rationale + no env override | `src/cli/commands/search/query.rs:128-132,682-686`, `src/cli/batch/handlers/search.rs:75-79,150-154` | ✅ PR #1046 |
| 101 | `Store::search_by_name` silently caps `limit` at 100 with no comment | `src/store/search.rs:86-92` | ✅ PR #1046 |
| 102 | `MAX_FTS_OUTPUT_LEN = 16384` silently truncates large chunks | `src/nl/fts.rs:105,145-150,158-162` | ✅ PR #1046 |
| 103 | `MAX_CALL_GRAPH_EDGES` / `MAX_TYPE_GRAPH_EDGES` 500K hardcoded, no env override | `src/store/calls/query.rs:87-104`, `src/store/types.rs:464-480` | ✅ PR #1046 |
| 104 | `parser/mod.rs::MAX_FILE_SIZE = 50MB` hard ceiling overrides `CQS_MAX_FILE_SIZE` | `src/parser/mod.rs:30,177,368` | ✅ PR #1046 |
| 105 | `MAX_CHUNK_BYTES = 100_000` silently drops large chunks; comment about windowing wrong | `src/parser/mod.rs:37-38,274-283` | ✅ PR #1046 |
| 106 | `convert/{chm,webhelp}::MAX_PAGES = 1000` duplicated, hardcoded, no env override | `src/convert/{chm,webhelp}.rs` | ✅ PR #1046 |
| 107 | `MAX_STDIN_SIZE` / `MAX_DIFF_SIZE` 50MB hardcoded, no env override | `src/cli/commands/mod.rs:476,512` | ✅ PR #1046 |
| 108 | `convert/mod.rs::MAX_WALK_DEPTH = 50` hardcoded, no env override, no warning on hit | `src/convert/mod.rs:493-505` | ✅ PR #1046 |
| 109 | `MAX_DAEMON_RESPONSE = 16 MiB` hardcoded — silent CLI fallback on large outputs | `src/cli/dispatch.rs:692-716` | ✅ PR #1046 |
| 110 | `write_json_line` Infinity coverage missing — only NaN tested in retry path | `src/cli/batch/mod.rs:1773` | ✅ PR #1046 |
| 111 | `wrap_value` no double-wrap defense / detection | `src/cli/json_envelope.rs:102-108` | ✅ PR #1046 |
| 112 | `cmd_reconstruct` (CLI) has zero integration tests | `src/cli/commands/io/reconstruct.rs:22-62` | ✅ PR #1046 |
| 113 | `cmd_drift` and `cmd_diff` have zero CLI integration tests | `src/cli/commands/io/drift.rs:77-150`, `src/cli/commands/io/diff.rs:82-125` | ✅ PR #1046 |
| 114 | `cmd_brief` has zero integration tests | `src/cli/commands/io/brief.rs:112-161` | ✅ PR #1046 |
| 115 | `cmd_neighbors` has zero integration tests | `src/cli/commands/search/neighbors.rs:155-194` | ✅ PR #1046 |
| 116 | `cmd_cache` subcommands have zero integration tests | `src/cli/commands/infra/cache_cmd.rs:35-138` | ✅ PR #1046 |
| 117 | `cmd_doctor --fix` and `cmd_init --force` have no integration test | `src/cli/commands/infra/doctor.rs:97+`, `src/cli/commands/infra/init.rs:13+` | ✅ PR #1046 |
| 118 | `cmd_telemetry_reset` non-atomic copy-then-truncate can lose telemetry log on crash | `src/cli/commands/infra/telemetry_cmd.rs:520-578` | ✅ PR #1046 |
| 119 | `apply_windowing` recomputes invariant `max_tokens_per_window` per chunk | `src/cli/pipeline/windowing.rs:36-44` | ✅ PR #1046 |
| 120 | Pipeline span numbering: same logical stage logged with two different numbers | `src/cli/batch/pipeline.rs:260-285` | ✅ PR #1046 |
| 121 | `fit_review_to_budget` "always keep at least one" cascade overshoots tight budget by ~50 tokens | `src/cli/commands/review/diff_review.rs:104-119` | ✅ PR #1046 |
| 122 | `Store::search_by_name` tie-breaker uses chunk id which sorts line numbers lexicographically | `src/store/search.rs:138-148` | ✅ PR #1046 |
| 123 | `BatchContext::file_set()` clones full HashSet on every call | `src/cli/batch/mod.rs:848-862` | ✅ PR #1046 |
| 124 | `QueryCache` (disk SQLite) has no max-size cap | `src/cache.rs:893-1099` | ✅ PR #1046 |
| 125 | `MAX_CONCURRENT_DAEMON_CLIENTS = 64` hardcoded — overprovisioned, no env override | `src/cli/watch.rs:83-88` | ✅ PR #1046 |
| 126 | `prepare_for_embedding` clones every cached embedding into HashMap then again into Vec | `src/cli/pipeline/embedding.rs:53-90` | ✅ PR #1046 |
| 127 | `EmbeddingCache::write_batch` invocation clones every content_hash + every embedding per batch | `src/cli/pipeline/embedding.rs:277-282,405-410` | ✅ PR #1046 |
| 128 | `code_types()` allocates fresh Vec on every search query — should be `LazyLock` | `src/language/mod.rs:732-734` | ✅ PR #1046 |
| 129 | `extract_types` clones every classified type_name into HashSet just for membership | `src/parser/calls.rs:181-189` | ✅ PR #1046 |
| 130 | Three call sites build SQL placeholder strings inline — bypassing cached `make_placeholders` | `src/store/calls/crud.rs:116-119`, `src/cache.rs:191-194`, `src/search/scoring/filter.rs:83,96,106` | ✅ PR #1046 |
| 131 | `cli/store.rs::open_project_store{,_readonly}` lack `tracing::info_span!` at entry | `src/cli/store.rs:18-47` | ✅ PR #1046 |
| 132 | `where_to_add::suggest_placement_with_options` lacks entry span; embedder errors propagate without context | `src/where_to_add.rs:117-131` | ✅ PR #1046 |
| 133 | `extract_doc_fallback_for_short_chunk` silently drops `source.get(..start_byte)?` failures | `src/parser/chunk.rs:289-310` | ✅ PR #1046 |
| 134 | `cli/telemetry.rs::log_command/log_routed` `let _ = (closure)()` discards file-open errors | `src/cli/telemetry.rs:66-124,169-224` | ✅ PR #1046 |
| 135 | `Store::list_stale_files` silently treats `metadata()` permission-denied as "fresh" | `src/store/chunks/staleness.rs:472-487` | ✅ PR #1046 |
| 136 | Telemetry `query` field captures full search query strings unredacted | `src/cli/telemetry.rs:53-63,231-255` | ✅ PR #1046 |
| 137 | `chat_history` file persists across sessions with default umask, captures every query | `src/cli/chat.rs:139,153,251` | ✅ PR #1046 |
| 138 | Daemon socket `args_preview` echoes file paths/snippets at debug log — privileged journal harvest | `src/cli/watch.rs:246-262` | ✅ PR #1046 |
| 139 | `cli::pipeline::parsing.rs::extract_doc_fallback`: extract_method_name miss for `proc`/`procedure`/`sub` (overlaps #57) | `src/nl/fields.rs:204-258` | ✅ PR #1046 |
| 140 | `parser/chunk::extract_doc_fallback_for_short_chunk` strips `\r` redundantly with `lines()` | `src/parser/chunk.rs:306` | ✅ PR #1046 |
| 141 | `enumerate_files` `to_ascii_lowercase` extension matching allocates per file | `src/lib.rs:518-527` | ✅ PR #1046 |
| 142 | `lib::normalize_path` does not strip Windows `\\?\` UNC prefix | `src/lib.rs:339-348` | ✅ PR #1046 |

## P4 — hard or low impact (defer / open issues)

| # | Title | Location | Status |
|---|---|---|---|
| 143 | `WINDOW_OVERHEAD = 32` tokens assumed-fits all model prefix configurations | `src/cli/pipeline/windowing.rs:8,12-18` | ✅ filed: #1042 |
| 144 | `Store::is_slow_mmap_fs` is `#[cfg(unix)]`-only — Windows network drives silently take 256 MB mmap × 4 conns | `src/store/mod.rs:371-405` | ✅ filed: #1043 |
| 145 | `ctrlc::set_handler` SIGINT-only — `cqs watch` on Windows cannot be cleanly stopped | `src/cli/signal.rs:27-37`, `src/cli/watch.rs:118-132` | ✅ filed: #1044 |
| 146 | `ChunkType::human_name` `_ => other.to_string()` catch-all hides multi-word omissions | `src/language/mod.rs:683-690` | ✅ filed: #1047 |
| 147 | `dispatch::try_daemon_query` deserializes `output` strictly as string — future structured payloads silently fall back | `src/cli/dispatch.rs:739-741` | ✅ filed: #1048 |
| 148 | `fallback_does_not_mix_comment_styles` test expectation undocumented | `src/parser/chunk.rs:264-276,315-322` | ✅ filed: #1049 (security/correctness obviated by P1 #3 — issue tracks the missing test pin only) |

## Execution log

All 150 findings addressed across 4 PRs + 3 issues (2026-04-18 → 2026-04-19):

- **P1** (26 items, easy + high impact) → **PR #1041** merged
- **P2** (47 items, medium effort + high impact) → **PR #1045** merged
- **P3** (69 items, easy + low impact) → **PR #1046** (audit-complete PR)
- **P4 hard** (3 items) → issues **#1042**, **#1043**, **#1044** filed for future Windows/model-flexibility work
- **P4 trivial** (3 items) → issues **#1047**, **#1048**, **#1049** filed for tracking; #1049 is partially obviated by P1 #3 (security/correctness aspect closed) but the test-pin remains an open ask

Wave logistics: 1 sequential P1 wave (mostly serial), 13 parallel P2 agents + 1 sweep agent for cross-cutting `parser_version` propagation, 5 parallel P3 agents.

**Lessons learned for the next audit**: cross-cutting struct field additions (e.g., the `parser_version` field added by P2 #29) need to be treated as a wave-0 prerequisite — give the field-adding work to a single agent, commit, then dispatch the rest. Doing them in parallel with other file-family agents creates ~50 missing-field cascade errors that need a follow-up sweep.
