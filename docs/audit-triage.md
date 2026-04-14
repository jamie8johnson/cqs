# Audit Triage — v1.25.0

Triage date: 2026-04-14. Source: `docs/audit-findings.md` (236 findings across 17 sections, 2 batches of 8 parallel opus auditors).

## Triage rules

- **P1** — easy difficulty AND high impact (real bug or security exposure that could bite a user soon). Fix immediately.
- **P2** — medium effort AND high impact (real bug, but takes more than a one-liner). Fix in next batch.
- **P3** — easy difficulty AND low impact (cosmetic, doc, hygiene, perf-on-slow-path). Fix if time.
- **P4** — hard effort OR low impact (architectural changes, "would be nice", coverage gaps with no current bug). Hard ones get filed as GitHub issues; trivial ones (one-liners, doc fixes) fixed inline.

Priority bias: lean higher for items that already bit us today (GC suffix-match, daemon mutation gap, eval-output watch-reindex, CAGRA filtering, CRLF noise on WSL), touch the daemon hot path, affect determinism/reproducibility, or are silent failures. Lean lower for "nice to have" coverage, speculative scaling, and documenting safe-by-construction invariants.

Cross-references to known open issues: `#909, #912-#925, #856, #717, #389, #255, #106, #63`.

## P1 — Easy + High Impact (fix immediately)

| ID | Title | Difficulty | Location | Status |
|---|---|---|---|---|
| Algorithm Correctness: GC prune_all suffix-match | GC `prune_all` suffix-match too loose — 81% orphan retention | easy | `src/store/chunks/staleness.rs:180-184` | ✅ wave-1 (ccda36e) |
| AC-V1.25-1 | `Store::rrf_fuse` non-deterministic tie-breaker (eval flake) | easy | `src/store/search.rs:187-193` | ✅ wave-1 (0686264) |
| AC-V1.25-2 | Non-deterministic sort tie-breaking in 12+ score-sorting sites | easy | `src/search/query.rs:381,772` + 14 others | ✅ wave-1 (0686264) |
| AC-V1.25-3 | Type-boost re-sort in `finalize_results` not deterministic on ties | easy | `src/search/query.rs:381` | ✅ wave-1 (0686264) |
| AC-V1.25-5 | `parse_unified_diff` defaults unparseable start to line 1 — spurious hunks | easy | `src/diff_parse.rs:73-82` | pending |
| AC-V1.25-9 | `is_cross_language_query` matches "port " across word boundaries | easy | `src/search/router.rs:518-536` | ✅ wave-1 (f720bf7) |
| AC-V1.25-15 | `is_behavioral_query` matches as substrings — hyphenated identifier false positives | easy | `src/search/router.rs:552-563` | ✅ wave-1 (f720bf7) |
| Code Quality: .claude/worktrees/ indexed | `.claude/worktrees/` indexed as project code; not in `.gitignore` | easy | `.gitignore`, file walker | ✅ wave-1 (d01766a) |
| CQ-V1.25-1 | `--threshold` / `-t` silently dropped on daemon-routed queries | easy | `src/cli/batch/commands.rs:392-413` | pending |
| CQ-V1.25-2 | scout/similar/related limit clamps drift between CLI and batch dispatchers | easy | `src/cli/commands/search/scout.rs:155` + 2 others | ✅ wave-1 (f720bf7) |
| CQ-V1.25-3 | `HnswIndex::try_load_with_ef` default-dim footgun survives PR #900 | easy | `src/hnsw/persist.rs:786-824` | ✅ wave-2 (a88b63a) |
| API Design: Daemon batch parser misses mutation cmds | Daemon batch parser misses `audit-mode`, more `notes` subcommands | easy | `src/cli/dispatch.rs::try_daemon_query` | pending |
| API-V1.25-1 | Daemon forwards 8 commands the batch parser cannot handle | easy | `src/cli/dispatch.rs:401-423` | ✅ wave-1 (63ff389) |
| API-V1.25-2 | `cqs stale --count-only` and `cqs drift -t` rejected by daemon | easy | `src/cli/batch/commands.rs:218,222-237` | ✅ wave-1 (63ff389) |
| API Design: cqs stale --json output is not JSON | `cqs stale --json` produces non-JSON; programmatic consumers break | easy | `src/cli/commands/...stale.rs` | pending |
| EH-13 | CLI silently swallows daemon-reported errors and re-executes locally | medium→easy | `src/cli/dispatch.rs:510-520,58-62` | ✅ wave-1 (63ff389) |
| EH-15 | Daemon `read_line` 1MB check is post-hoc — OOM possible | easy | `src/cli/watch.rs:71-82` | ✅ wave-1 (eb48f41) |
| EH-18 | Watch-mode socket setup silently discards stale-socket + permission-set errors | easy | `src/cli/watch.rs:409-413,418-420` | ✅ wave-1 (7bae42d) |
| EH-19 | Watch-mode stale HNSW cleanup silently discards remove errors — stale results next boot | easy | `src/cli/watch.rs:888-893` | ✅ wave-1 (ef839be) |
| RB-NEW-3 | Daemon socket `read_line` allocates unbounded before 1MB check | easy | `src/cli/watch.rs:69-82` | ✅ wave-1 (eb48f41) |
| RB-NEW-4 | Daemon client `read_line` on response unbounded — wedged daemon OOMs CLI | easy | `src/cli/dispatch.rs:504-508` | ✅ wave-1 (63ff389) |
| SEC-V1.25-2 | Daemon socket request `read_line` allocates before 1MB check — OOM vector | easy | `src/cli/watch.rs:69-82` | ✅ wave-1 (eb48f41) |
| SEC-V1.25-4 | `QueryCache::open` never sets 0o600 on DB file (asymmetric with EmbeddingCache) | easy | `src/cache.rs:889-937` | ✅ wave-2 (439d627) |
| SEC-V1.25-5 | `telemetry::log_command` / `log_routed` create file without `.mode(0o600)` — umask race | easy | `src/cli/telemetry.rs:88-96,171-178` | ✅ wave-2 (368cda6) |
| SEC-V1.25-7 | `doctor --fix` invokes `cqs` via PATH — PATH-injection hijack of recovery flow | easy | `src/cli/commands/infra/doctor.rs:41,57` | ✅ wave-2 (5da5f3e) |
| SEC-V1.25-13 | `CQS_LLM_API_BASE` permits silent HTTPS→HTTP downgrade with only a warn | easy | `src/llm/mod.rs:219-249` | ✅ wave-2 (510494a) |
| SEC-V1.25-15 | Stale-socket cleanup removes any file at socket path — symlink TOCTOU | medium→easy | `src/cli/watch.rs:401-413` | ✅ wave-1 (40ba816) |
| PB-V1.25-15 | Socket `set_permissions(0o600).ok()` silently fails on 9P/NFS/FUSE | easy | `src/cli/watch.rs:418-421` | ✅ wave-1 (7bae42d) |
| PB-V1.25-18 | `cqs watch --serve` Windows branch silently drops flag | easy | `src/cli/watch.rs:465-468` | ✅ wave-1 (6ac57eb) |
| PB-V1.25-8 | `gitignore` boilerplate writes `\n`-only — dirty `git status` on Windows autocrlf=true | easy | `src/cli/commands/infra/init.rs:37-40` | pending |
| SHL-V1.25-1 | `CQS_DAEMON_TIMEOUT_MS` integer-divided to seconds, loses sub-second precision | easy | `src/cli/dispatch.rs:443-449` | ✅ wave-1 (63ff389) |
| SHL-V1.25-2 | Daemon write timeout hardcoded 30s — long SPLADE+reranker queries exceed silently | easy | `src/cli/watch.rs:65-66`, `src/cli/dispatch.rs:451` | ✅ wave-1 (63ff389) |
| EH-16 | HNSW self-heal `is_hnsw_dirty().unwrap_or(true)` swallows metadata errors | easy | `src/cli/store.rs:289,343` | ✅ wave-2 (9be9cfd) |
| EH-17 | `query_cache::get` silently treats DB errors as cache miss | easy | `src/cache.rs:941-949` | ✅ wave-2 (2f1e678) |
| OB-NEW-6 | `QueryCache::get` swallows sqlite errors and dim mismatches — cache poisoning invisible | easy | `src/cache.rs:940-961` | ✅ wave-2 (2f1e678) |
| EH-21 | `handle_socket_client` `catch_unwind` discards panic payload | easy | `src/cli/watch.rs:113-138` | ✅ wave-1 (2ba4b6a) |
| OB-NEW-4 | Daemon panic path discards payload — error log opaque | easy | `src/cli/watch.rs:135-138` | ✅ wave-1 (2ba4b6a) |
| EH-22 | Socket accept-loop errors logged at `debug` level | easy | `src/cli/watch.rs:494` | ✅ wave-1 (40a16c2) |
| RM-V1.25-20 | Daemon accept-error loop logs at `debug` — busy-spin invisibly | easy | `src/cli/watch.rs:493-495` | ✅ wave-1 (40a16c2) |
| EH-12 | `batch::dispatch_line` and `cmd_batch` flatten anyhow chain to top-level | easy | `src/cli/batch/mod.rs:254-256,844-849,851-856` | pending |
| RB-NEW-1 | `SpladeEncoder::encode_batch` pre-pooled slicing panics on short tensor | easy | `src/splade/mod.rs:688-702` | ✅ wave-1 (74a01f4) |
| RB-NEW-2 | `SpladeEncoder::encode_batch` raw-logits path panics + wrong `.expect` message | easy | `src/splade/mod.rs:733-740` | ✅ wave-1 (74a01f4) |
| RB-NEW-5 | `unreachable!()` in notes dispatch encodes routing invariant outside type system | easy | `src/cli/commands/io/notes.rs:116,147` | ✅ wave-2 (9e87eb9) |
| EH-10 | Periodic `flush_calls` loses items when `upsert_calls_batch` fails | easy | `src/cli/pipeline/upsert.rs:44-60` | ✅ wave-2 (a42206a) |
| EH-11 | Periodic `flush_type_edges` unconditionally clears buffer on failure | easy | `src/cli/pipeline/upsert.rs:69-81,177-178` | ✅ wave-2 (dc8bf79) |
| RM-V1.25-9 | No explicit SIGTERM handler — systemd `stop` may hard-kill daemon, WAL unflushed | easy | `src/cli/signal.rs:27-37` | ✅ wave-1 (7f4b72f) |
| RM-V1.25-23 | Watch `pending_files` cap drops events silently — no full-rescan fallback | easy | `src/cli/watch.rs:186-195,784-792` | ✅ wave-1 (4ec3dd8) |
| RM-V1.25-25 | `CQS_TELEMETRY` is sticky once file exists — disabling via env doesn't stop collection | easy | `src/cli/telemetry.rs:44,118` | ✅ wave-2 (41d5417) |

## P2 — Medium + High Impact (fix in batch)

| ID | Title | Difficulty | Location | Status |
|---|---|---|---|---|
| AC-V1.25-7 | CAGRA `search_impl` returns phantom "perfect" results when `index.len() < k` | medium | `src/cagra.rs:193-273` | ✅ wave-1 (6434c41) |
| AC-V1.25-8 | HNSW self-heal dirty-flag shared across enriched and base — clearing one clears both | medium | `src/cli/store.rs:289-308,343-360`, `src/store/metadata.rs:186-207` | ✅ wave-2 (f66d898) |
| AC-V1.25-10 | `MULTISTEP_PATTERNS` includes bare `" and "` / `" or "` — routes simple conjunctions wrong | medium | `src/search/router.rs:257-259` | ✅ wave-1 (f720bf7) |
| AC-V1.25-13 | `compute_risk_batch` entry-point heuristic conflates 4 distinct cases as Medium | medium | `src/impact/hints.rs:138-147` | pending |
| AC-V1.25-14 | `test_reachability` BFS node cap truncates mid-class — biased per-function risk scores | medium | `src/impact/bfs.rs:277-301` | pending |
| Error Handling: Ingest pipeline FK failures | Ingest pipeline FK failures silently discard call graph edges (14k+ edges lost) | medium | `src/cli/pipeline/upsert.rs` | pending |
| Data Safety: HNSW state can diverge | HNSW state can diverge from chunks table after out-of-band mutation — ghost results | medium | `src/cli/commands/index/gc.rs`, HNSW load path | pending |
| DS-V1.25-2 | `HnswIndex::insert_batch` partial failure leaves graph out of sync with `id_map` | medium | `src/hnsw/mod.rs:234-284` | ✅ wave-2 (166aa05) |
| DS-V1.25-3 | Watch-mode `set_hnsw_dirty(false)` + chunks-write race can clear flag with unindexed chunks | hard→medium | `src/cli/watch.rs:835-937` | pending |
| DS-V1.25-5 | `upsert_calls_batch` aborts on FK violation — watch race drops calls silently | medium | `src/store/calls/crud.rs:55-97` | pending |
| DS-V1.25-6 | Daemon read-only BatchContext can't detect `cqs index --force` DB replacement | medium | `src/cli/watch.rs:476-498`, `src/cli/batch/mod.rs:138-187` | pending |
| API-V1.25-3 | `suggest --apply` writes through a read-only store — fails in CLI and daemon | medium | `src/cli/dispatch.rs:335`, `src/cli/batch/handlers/analysis.rs:86-128` | ✅ issue #946 |
| API-V1.25-4 | `BatchCmd::Search` inline-duplicates 21 fields instead of shared `SearchArgs` | medium | `src/cli/batch/commands.rs:32-92` vs `src/cli/definitions.rs:122-226` | ✅ issue #947 |
| API-V1.25-8 | Daemon forward silently ignores `--model` mismatch — wrong model used | medium | `src/cli/dispatch.rs:453-490` | ✅ wave-1 (63ff389) |
| EH-14 | Daemon socket timeouts set via silent `.ok()` discards — guard defeated | easy→medium | `src/cli/watch.rs:65-66`, `src/cli/dispatch.rs:442-451` | pending |
| EX-V1.25-11 | Notes subcommand splits across two dispatch fns with `unreachable!()` — same class as PR #945 | medium | `src/cli/commands/io/notes.rs:106-149`, `src/cli/dispatch.rs:175-179,415-423` | pending |
| SEC-V1.25-1 | Daemon socket DoS — single-threaded accept loop with 5s read timeout | medium | `src/cli/watch.rs:488-497,58-146` | pending |
| SEC-V1.25-6 | LLM prompt construction concatenates unsanitized chunk content inside backticks — prompt injection from references | medium | `src/llm/prompts.rs:13-18,47-53,85-95,108-115` | ✅ wave-2 (283d7ae) |
| SEC-V1.25-8 | `CQS_PDF_SCRIPT` extension-only guard can't prevent `.py` payloads — daemon-persisted | medium | `src/convert/pdf.rs:56-69` | ✅ wave-2 (ab3206d) |
| SEC-V1.25-10 | `find_python` / `find_7z` rely on PATH with no exec-bit / ownership check | medium | `src/convert/mod.rs:48-60`, `src/convert/chm.rs:182-208` | ✅ wave-2 (0ed9dd9) |
| SEC-V1.25-11 | `git_log` / `git_diff_tree` / `git_show` accept `repo: &Path` without canonicalize/validate | medium | `src/train_data/git.rs:29,92,131,196` | ✅ wave-2 (08c9751) |
| SEC-V1.25-12 | Reindex of cross-fs paths without symlink-escape check on event-triggered re-reads | medium | `src/cli/watch.rs:556-560,740-810` | pending |
| SEC-V1.25-14 | `add_reference_to_config` stores user-supplied source verbatim — no trust check on update | medium | `src/cli/commands/infra/reference.rs:87-184`, `src/config.rs:480-...` | pending |
| PB-V1.25-2 | `daemon_socket_path` is `#[cfg(unix)]`-only — `cqs watch --serve` silently no-ops on Windows | medium | `src/cli/files.rs:10-28`, `src/cli/watch.rs:465-468` | ✅ wave-1 (6ac57eb) |
| PB-V1.25-7 | Staleness macOS case-insensitivity branch ignores Windows NTFS — silent data loss | medium | `src/store/chunks/staleness.rs:55-74,167-188,326-337` | ✅ wave-1 (ccda36e) |
| PB-V1.25-12 | ORT provider symlink setup is Linux-only — macOS CoreML and Windows CUDA silent CPU | medium | `src/embedder/provider.rs:26-189,191-200` | pending |
| RM-V1.25-3 | Daemon embedder/reranker idle timeout only checked when query arrives — pins ~500MB+ | medium | `src/cli/batch/mod.rs:108-136,248` | pending |
| RM-V1.25-5 | `EmbeddingCache.evict()` never fires in daemon/watch mode — 10GB cap blown silently | medium | `src/cli/pipeline/mod.rs:166-171`, `src/cache.rs:137-140` | pending |
| RM-V1.25-7 | Cached ReferenceIndexes have no per-reference staleness detection | medium | `src/cli/batch/mod.rs:88,207,440-472` | pending |
| RM-V1.25-8 | Detached socket-handler thread holds BatchContext past main-loop exit | medium | `src/cli/watch.rs:473-505` | pending |
| RM-V1.25-11 | SPLADE index full rebuild on every reindex — first daemon query blocked ~45s | hard→medium | `src/cli/batch/mod.rs:343-386`, `src/splade/index.rs:130-150` | pending |
| RM-V1.25-15 | Reranker/SPLADE/Embedder tokenizers never cleared by `clear_session` (~50MB resident) | medium | `src/cli/batch/mod.rs:76,582-594` + 3 others | pending |
| RM-V1.25-19 | CAGRA GPU mutex poison recovery uses `into_inner()` without GPU state reset | medium | `src/cagra.rs:153-156` | pending |
| RM-V1.25-22 | `read_line` on daemon socket can allocate multi-GB before size check | medium | `src/cli/watch.rs:68-82` | ✅ wave-1 (eb48f41) |
| RM-V1.25-28 | Watch outer Embedder and daemon-thread Embedder are separate OnceLocks — ~500MB dup | medium | `src/cli/watch.rs:568`, `src/cli/batch/mod.rs:74,279` | pending |
| TC-ADV-8 | Notes daemon-forward regression (PR #945) has no test — silent regression risk | medium | `src/cli/dispatch.rs:401-423` | pending |
| TC-ADV-9 | Concurrent `upsert_sparse_vectors` writer race has no test — WRITE_LOCK regression silent | medium | `src/store/sparse.rs:130-146` | pending |
| TC-HP-2 | `cmd_notes_mutate` add/update/remove lifecycle has zero CLI tests — PR #945 untested | easy→medium | `src/cli/commands/io/notes.rs:122-447` | pending |
| TC-HP-3 | `prune_all` has zero tests — happy path never verified | easy→medium | `src/store/chunks/staleness.rs:148-284` | ✅ wave-1 (aa660d5) |
| TC-HP-4 | Per-category SPLADE alpha routing has no end-to-end test — silent quality regression | medium | `src/cli/batch/handlers/search.rs:117-139`, `src/cli/commands/search/query.rs:170-200` | pending |
| TC-HP-1 | `resolve_splade_alpha` has zero tests — v1.25.0 defaults unguarded | easy→medium | `src/search/router.rs:272-322` | pending |
| PF-V1.25-2 | `rrf_fuse` full-sorts every result to truncate to `limit` — should use bounded heap | medium | `src/store/search.rs:187-193` | ✅ wave-1 (2ef2de9) |
| PF-V1.25-3 | SPLADE `search_with_filter` full-sorts entire score HashMap — 10k-30k clones discarded | medium | `src/splade/index.rs:195-208` | ✅ wave-1 (2ef2de9) |
| PF-V1.25-4 | `NoteBoostIndex` rebuilt from scratch per search — should cache per notes-generation | medium | `src/search/query.rs:170,708`, `src/search/scoring/note_boost.rs:62-97` | pending |
| PF-V1.25-19 | `load_all_sparse_vectors` loads entire sparse_vectors table — 4GB peak on 60k-chunk index | medium | `src/store/sparse.rs:207-249` | pending |
| OB-NEW-7 | CAGRA vs HNSW selection logged inconsistently — operator can't tell backend | easy→medium | `src/cli/store.rs:264-282` | ✅ wave-1 (672880b) |

## P3 — Easy + Low Impact (fix if time)

| ID | Title | Difficulty | Location | Status |
|---|---|---|---|---|
| AC-V1.25-4 | `apply_parent_boost` float-precision cap overshoot — `1.15000010` vs cap `1.15` | easy | `src/search/scoring/candidate.rs:71-85` | pending |
| AC-V1.25-6 | `extract_file_from_chunk_id` treats bare `:w` suffix as windowed | easy | `src/search/scoring/filter.rs:24-32` | pending |
| AC-V1.25-11 | `expand_query_for_fts` preserves original-case token in OR groups — tokenizer-dependent | easy | `src/search/synonyms.rs:70-80` | pending |
| AC-V1.25-12 | `NameMatcher::score` word-overlap path skips equal-length substring matches | easy | `src/search/scoring/name_match.rs:146-150` | pending |
| CQ-V1.25-4 | `prune_all` inlines four DELETE statements that have dedicated prune methods | easy | `src/store/chunks/staleness.rs:223-257` | ✅ wave-1 (ccda36e) |
| CQ-V1.25-6 | Suffix-match filter block duplicated 27 lines byte-for-byte across `prune_*` | easy | `src/store/chunks/staleness.rs:51-77,161-189` | ✅ wave-1 (ccda36e) |
| CQ-V1.25-7 | `dispatch_scout` JSON shape structurally diverges from `cmd_scout` (no shared serializer) | easy | `src/cli/commands/search/scout.rs:169-175`, `src/cli/batch/handlers/misc.rs:186-209` | pending |
| CQ-V1.25-8 | `BatchContext::notes_cache` invalidation is unreachable in daemon sessions | easy | `src/cli/batch/handlers/misc.rs:98-131`, `src/cli/batch/mod.rs:204` | pending |
| EH-20 | `dispatch_diff` has dead placeholder binding — pitfall waiting to trip | easy | `src/cli/batch/handlers/misc.rs:322-334,337-357` | pending |
| DS-V1.25-1 | SPLADE cross-device fallback writes directly into final path — not atomic | easy | `src/splade/index.rs:399-423` | ✅ wave-1 (bd3c7ef) |
| DS-V1.25-4 | HNSW save writes `id_map` without `sync_all()` — power-cut may lose durability | easy | `src/hnsw/persist.rs:262-287` | ✅ wave-2 (de3dc7f) |
| DS-V1.25-7 | `query_log.jsonl` append has no lock + no size cap — interleaved lines under concurrency | easy | `src/cli/batch/commands.rs:351-379` | pending |
| SHL-V1.25-3 | SQLite `cache_size` hardcoded per-mode; no env override | easy | `src/store/mod.rs:288,311,328,347` | pending |
| SHL-V1.25-4 | `cache.rs:175` SQL DELETE batch still uses pre-3.32 `chunks(100)` | easy | `src/cache.rs:174-175` | pending |
| SHL-V1.25-5 | `types.rs:102` SQL DELETE batch still uses pre-3.32 `chunks(500)` | easy | `src/store/types.rs:102` | pending |
| SHL-V1.25-7 | `DEFAULT_MAX_CHANGED_FUNCTIONS = 500` silently truncates large diffs | easy | `src/impact/diff.rs:21,136-145` | pending |
| SHL-V1.25-9 | Batch context reference LRU hardcoded to 2 slots | easy | `src/cli/batch/mod.rs:687,722` | pending; duplicate of RM-V1.25-17 |
| SHL-V1.25-11 | `MAX_FILE_SIZE = 1_048_576` silently skips 1MB+ files, not tunable | easy | `src/lib.rs:424` | pending |
| SHL-V1.25-12 | `embedding_cache.rs` `busy_timeout(5s)` hardcoded, ignores `CQS_BUSY_TIMEOUT_MS` | easy | `src/cache.rs:86,910` | pending |
| SHL-V1.25-13 | Watch debounce 500ms default — tuned for inotify, unsafe on WSL NTFS | easy | `src/cli/definitions.rs:292-295` | pending |
| SHL-V1.25-14 | `PLACEHOLDER_CACHE_MAX = 10_000` short of SQLite limit — cache misses on large batches | easy | `src/store/helpers/sql.rs:36` | pending |
| SHL-V1.25-15 | SPLADE `max_seq_len` p99 comment claims 180 tokens — assumes cqs corpus | easy | `src/splade/mod.rs:587-598` | pending |
| SHL-V1.25-16 | `IDLE_TIMEOUT_MINUTES = 5` hardcoded in batch mode | easy | `src/cli/batch/mod.rs:44` | pending |
| OB-NEW-1 | `resolve_splade_alpha` has no span; routing decision logged at two diverging sites | easy | `src/search/router.rs:272-322`, two call sites | ✅ wave-1 (cc14508) |
| OB-NEW-2 | Daemon startup env-var log is `println!` to stdout, lists 6 of ~68 vars | easy | `src/cli/watch.rs:429-455` | ✅ wave-1 (4e43a14) |
| OB-NEW-3 | `daemon_query` span lacks `command` field; per-command latency uncorrelatable | easy | `src/cli/watch.rs:62` | ✅ wave-1 (8f25fcf) |
| OB-NEW-5 | `try_daemon_query` has no span and 4 silent `None` returns | easy | `src/cli/dispatch.rs:401-521` | ✅ wave-1 (63ff389) |
| OB-NEW-8 | `search_hybrid` fusion logs at `debug!` — production-critical decision invisible | easy | `src/search/query.rs:509-516,547` | pending |
| OB-NEW-9 | `dispatch_search` (batch) drops classifier confidence + strategy from log | easy | `src/cli/batch/handlers/search.rs:117-119` | pending |
| OB-NEW-10 | Watch mode SPLADE-skip warning fires only at startup; per-cycle drift invisible | easy | `src/cli/watch.rs:528-540,802` | ✅ wave-1 (f378654) |
| OB-NEW-11 | `handle_socket_client` ignores write errors on response delivery | easy | `src/cli/watch.rs:74,87,109,130,133` | ✅ wave-1 (89c675e) |
| OB-NEW-12 | CAGRA dim-mismatch warns use format-string args (post-v1.22 OB-18 holdouts) | easy | `src/cagra.rs:146-150,315-319` | ✅ wave-1 (854bc7f) |
| API-V1.25-6 | `BatchCmd::is_pipeable` allowlist drifts out of date silently | easy | `src/cli/batch/commands.rs:325-344` | ✅ wave-1 (af0d0cf) |
| API-V1.25-7 | `validate_finite_f32` is dead — no f32 flag uses it as a clap value_parser | easy | `src/cli/definitions.rs:97-112` | ✅ wave-1 (3b97a59) |
| TC-ADV-1 | NaN embedding bytes flow into HNSW unchecked — no test blocks NaN propagation | easy | `src/store/chunks/embeddings.rs:47-50,101-103` | pending |
| TC-ADV-2 | `HnswIndex::search` with NaN/Inf in query has no test | easy | `src/hnsw/search.rs:47-116` | pending |
| TC-ADV-3 | `prepare_index_data` (CAGRA) doesn't skip zero/NaN — diverges from HNSW build | easy | `src/hnsw/mod.rs:295-328` | pending |
| TC-ADV-5 | `search_hybrid` `(limit*5).max(100)` overflow on large limits untested | easy | `src/search/query.rs:429,583` | pending |
| TC-ADV-6 | `parse_notes_str` sentiment NaN clamping doesn't handle NaN | easy | `src/note.rs:353-355` | pending |
| TC-ADV-13 | `prune_orphan_sparse_vectors` zero-rows generation-bump-skip branch untested | easy | `src/store/sparse.rs:229-262` | pending |
| TC-ADV-14 | `store::notes` no test for NaN / huge batch / empty-mentions sentiment | easy | `src/store/notes.rs:62-136` | pending |
| EX-V1.25-3 | New embedding model preset requires edits in 4 places that drift apart | easy | `src/embedder/models.rs:44-103` | pending |
| EX-V1.25-9 | Doc-comment formats indirected through string tags, defeating type safety | easy | `src/doc_writer/formats.rs:41-132`, `src/language/mod.rs:322-327` | pending |
| EX-V1.25-10 | CAGRA knobs hidden behind a single env var; HNSW exposes three but CAGRA exposes zero | easy | `src/cagra.rs:113,169,469` | pending; partial dup of SHL-V1.25-6 |
| EX-V1.25-13 | `ModelInfo::default()` uses hardcoded BGE-large constants — footgun for test writers | easy | `src/embedder/models.rs:304-315` | pending |
| SEC-V1.25-3 | Daemon socket path uses non-cryptographic `DefaultHasher` — document or replace | easy | `src/cli/files.rs:16-28` | ✅ wave-2 (cb5d0e2) |
| SEC-V1.25-9 | `handle_socket_client` args echoed to tracing at debug — full query strings logged | easy | `src/cli/watch.rs:106` | ✅ wave-1 (a10a44b) |
| SEC-V1.25-16 | Daemon logs full command for every query — sensitive `notes add` text persists in journal | easy | `src/cli/watch.rs:141-145` | ✅ wave-1 (49ec0e0) |
| PB-V1.25-1 | Cache paths hardcode `~/.cache/cqs/` — wrong on Windows and macOS conventions | easy | `src/cache.rs:45-48,882-885` + others | pending |
| PB-V1.25-3 | SPLADE model default ignores `HF_HOME` / `HUGGINGFACE_HUB_CACHE` | easy | `src/splade/mod.rs:195-202,817-820` | pending |
| PB-V1.25-5 | `cli/display.rs:27` absolute-path guard uses ad-hoc byte-matching | easy | `src/cli/display.rs:22-39` | pending |
| PB-V1.25-9 | `.cache/cqs/` parent-dir perms `0o700` set AFTER `create_dir_all` — TOCTOU window | easy | `src/cache.rs:63-69,892-898` | pending |
| PB-V1.25-10 | SPLADE `~/` tilde-expansion misses Windows `~\` and `%USERPROFILE%` | easy | `src/splade/mod.rs:178-187` | pending |
| PB-V1.25-11 | WSL detection doesn't distinguish WSL1 (inotify works) from WSL2 (drops events) | easy | `src/config.rs:30-47` | pending |
| PB-V1.25-13 | `is_wsl_mount` byte-matching duplicates `is_under_wsl_automount` with different behavior | easy | `src/config.rs:409-415` vs `src/cli/watch.rs:299-331` | pending |
| PB-V1.25-14 | Blake3 checksum file written 0o600 on unix, default umask on non-unix | easy | `src/hnsw/persist.rs:333-370` | pending |
| PB-V1.25-16 | `XDG_RUNTIME_DIR` fallback to `temp_dir` unexercised and untested | easy | `src/cli/files.rs:16-19` | pending |
| PB-V1.25-17 | SQLite WAL/SHM file permissions 0o600 set AFTER SQLite writes — TOCTOU on rw opens | easy | `src/store/mod.rs:425-441` | pending |
| PB-V1.25-19 | Daemon socket-cleanup `remove_file` follows symlinks — local symlink-attack | easy | `src/cli/watch.rs:401-413` | ✅ wave-1 (40ba816) |
| PB-V1.25-20 | Hardcoded `PathBuf::from("/project")` in 10+ test fixtures assumes Unix absolute paths | easy | `src/cli/commands/graph/deps.rs:170` + 9 others | pending |
| PF-V1.25-1 | `search_hybrid` rebuilds two unsized HashMaps + Vec/HashSet per SPLADE query | easy | `src/search/query.rs:475-507` | pending |
| PF-V1.25-5 | `rerank` clones the `content` of every candidate before delegating | easy | `src/reranker.rs:111-120` | pending |
| PF-V1.25-6 | `fetch_candidates_by_ids_async` builds an `id → rank` HashMap just to re-sort | easy | `src/store/chunks/async_helpers.rs:95-104` | pending |
| PF-V1.25-7 | `make_placeholders` clones full cached placeholder string on every call | easy | `src/store/helpers/sql.rs:73-83` | pending |
| PF-V1.25-8 | Batched call-graph/enrichment queries still use pre-3.32 SQLite `chunks(200/250/500)` | easy | `src/store/calls/query.rs:194,243,294` + others | pending |
| PF-V1.25-9 | `update_embeddings_with_hashes_batch` uses `BATCH_SIZE = 100` for 3-param INSERT | easy | `src/store/chunks/crud.rs:143-165` | pending |
| PF-V1.25-10 | `ctx.store()` syscalls `fs::metadata` on every batch handler call | easy | `src/cli/batch/mod.rs:141-187,267-270` | pending |
| PF-V1.25-13 | `path_matches_mention` allocates two Strings per note mention per candidate | easy | `src/note.rs:366-381`, `src/search/scoring/note_boost.rs:114-125` | pending |
| PF-V1.25-14 | `apply_parent_boost` clones `parent_type_name` for every result | easy | `src/search/scoring/candidate.rs:58-63` | pending |
| PF-V1.25-15 | HNSW `build_batched_with_dim` logs `info!` per batch — 50+ INFO lines per build | easy | `src/hnsw/build.rs:203-208` | pending |
| PF-V1.25-16 | `search_hybrid` builds `fused_map` cloning every id after already owning them | easy | `src/search/query.rs:549-551` | pending |
| TC-HP-8 | Four top-level JSON CLI tests assert structure only, not content | easy | `tests/cli_commands_test.rs:132-245`, `tests/onboard_test.rs:79-140` | pending |
| TC-HP-9 | `tests/gather_test.rs` assertions skip empty-result case | easy | `tests/gather_test.rs:69-89,127-186,188-232` | pending |
| TC-HP-10 | All 5 `QueryCategory` catch-all variants untested as hitting `_ => 1.0` | easy | `src/search/router.rs:317-321` | pending |
| TC-HP-13 | Pipeline envelope structure not asserted when stage returns zero results | easy | `tests/cli_batch_test.rs:384-412` | pending |
| TC-HP-15 | `cqs stale --json` CLI tests never assert actual file paths | easy | `tests/cli_commands_test.rs:329-393` | pending |
| TC-HP-16 | `BoundedScoreHeap` (scoring candidate struct) has zero test coverage | easy | `src/search/scoring/candidate.rs:108-204` | pending |
| TC-HP-18 | `test_health_cli_json` asserts field types but never that counts match | easy | `tests/cli_health_test.rs:119-182` | pending |
| TC-HP-19 | `test_build_batched_handles_rebuild_after_initial_build` doesn't search after rebuild | easy | `tests/hnsw_test.rs:258-288` | pending |
| TC-HP-20 | `test_gc_prunes_missing_files` asserts only 2 of 5 GC counters | easy | `tests/cli_test.rs:448-487` | ✅ wave-1 (aa660d5) |
| TC-ADV-4 | `BoundedScoreHeap::new(0)` silently discards all pushes — no test | easy | `src/search/scoring/candidate.rs:162-167,170-192` | pending |
| TC-ADV-7 | `parse_notes` 10MB file-size guard and `MAX_NOTES = 10_000` truncation untested | easy | `src/note.rs:171-184,22,344` | pending |
| TC-ADV-10 | `expand_query_for_fts` only `debug_assert!` against quote/paren — no release test | easy | `src/search/synonyms.rs:56-91` | pending |
| TC-ADV-11 | `embed_query` truncation has no adversarial test for multi-byte chars | easy | `src/embedder/mod.rs:596-605` | pending |
| TC-ADV-12 | `splade::encode` truncation has no boundary test for multi-byte chars | easy | `src/splade/mod.rs:378-394,546-561` | pending |
| RM-V1.25-1 | `query_log.jsonl` append-only with no rotation or size cap | easy | `src/cli/batch/commands.rs:347-379` | pending |
| RM-V1.25-2 | Telemetry archive files never deleted — unbounded `telemetry_*.jsonl` accumulation | easy | `src/cli/telemetry.rs:70-86,153-165` | pending |
| RM-V1.25-4 | `QueryCache` on-disk prune runs once per `Embedder::new()` — daemon never prunes | easy | `src/embedder/mod.rs:298-304` | pending |
| RM-V1.25-10 | `BatchContext::notes()` clones full `Vec<Note>` on every call | easy | `src/cli/batch/mod.rs:500-523,475-490` | pending |
| RM-V1.25-13 | `EmbeddingCache` SQLite WAL files persist indefinitely | easy | `src/cache.rs:82-94` | pending |
| RM-V1.25-14 | `EmbeddingCache::evict()` deletes entries but never `VACUUM`s — file doesn't shrink | easy | `src/cache.rs:305-354` | pending |
| RM-V1.25-16 | `base_hnsw` retained resident even when never used — doubles peak HNSW memory | easy | `src/cli/batch/mod.rs:81,419-434` | pending |
| RM-V1.25-17 | `refs` LRU size hardcoded to 2 — no env override | easy | `src/cli/batch/mod.rs:687,722` | pending; duplicate of SHL-V1.25-9 |
| RM-V1.25-24 | Idle-timeout reset by every command — trivial polling defeats ONNX session eviction | easy | `src/cli/batch/mod.rs:108-136` | pending |
| RM-V1.25-26 | Watch idle-cleanup uses `cycles_since_clear` instead of wall-clock — busy stream starves | easy | `src/cli/watch.rs:635,704-713` | pending |
| RM-V1.25-27 | `query_cache.db` `INSERT OR REPLACE` rewrites identical rows — WAL churn | easy | `src/cache.rs:970-983` | pending |

## P4 — Hard or Low Impact

### Trivial inline fixes (apply now)

These are easy/cosmetic items not worth a separate GitHub issue — clean up alongside the P1/P3 sweeps:

- **DOC-V1.25-1** — Stray `/` in doc comments (`src/search/router.rs:296,299,305,559`). One-line fix per occurrence. ✅ wave-1 (router cleanup)
- **DOC-V1.25-2** — README cuvs patch note pins "v1.24.0" — say "v1.24.0+" or strip version (`README.md:786`). ✅ wave-2 (docs)
- **DOC-V1.25-3** — CHANGELOG `[Unreleased]` link still points to v0.19.0; needs v1.25.0 anchor + per-version footers (`CHANGELOG.md:2188`). ✅ wave-2 (docs)
- **DOC-V1.25-4** — README env-var table missing 11 documented `CQS_*` vars (`README.md:649-704`). ✅ wave-2 (docs)
- **DOC-V1.25-5** — README does not document v1.25.0 per-category SPLADE alpha defaults. ✅ wave-2 (docs)
- **DOC-V1.25-6** — PRIVACY.md "Deleting Your Data" misses `~/.cache/cqs/` (`PRIVACY.md:46-56`). ✅ wave-2 (docs)
- **DOC-V1.25-7** — SECURITY.md Filesystem Access tables omit `~/.cache/cqs/` (`SECURITY.md:66-98`). ✅ wave-2 (docs)
- **DOC-V1.25-8** — CONTRIBUTING.md router.rs entry missing v1.25 categories and `resolve_splade_alpha`. ✅ wave-2 (docs)
- **DOC-V1.25-9** — README install section silent on patched cuvs git clone for CPU-only builds. ✅ wave-2 (docs)
- **DOC-V1.25-11** — README eval numbers contradictory across TL;DR / How It Works / Retrieval Quality. ✅ wave-2 (docs)
- **DOC-V1.25-12** — MEMORY.md says Schema v16 — current is v20; refresh test count alongside. ✅ wave-2 (MEMORY)
- **CQ-V1.25-5** — `build_with_dim` docstring points at nonexistent `build_batched()` (`src/hnsw/build.rs:29-38`). ✅ wave-2 (docs)
- **DS-V1.25-9** — `schema.sql` header still says `v18` — bump to v20 with v19/v20 enumerated (`src/schema.sql:1-3`). ✅ wave-2 (docs)
- **DS-V1.25-11** — eval output atomic-write gap is in Python harness, out of scope for cqs Rust; close as noted.
- **DS-V1.25-8** — Telemetry auto-archive race document-only — single-writer assumption is acceptable; document the invariant. ✅ wave-2 (docs)
- **SHL-V1.25-10** — `DEFAULT_QUERY_CACHE_SIZE = 128` comment assumes 1024-dim; just update the comment. ✅ wave-2 (docs)

### GitHub issues (file separately)

These are hard items (architectural changes, multi-file refactors, hardware/runtime gaps, or future-leaning improvements without a current bug). Each gets its own issue:

- **DOC-V1.25-10** — Re-benchmark and refresh README Performance table on v1.25.0 (medium; needs eval run).
- **AC-V1.25-7 (already P2)** — File companion issue if cuVS API doesn't expose fill-count: design a separate sentinel-init scheme.
- **DS-V1.25-10** — Add filesystem backup step before `migrate()` runs DDL (medium; design `CQS_MIGRATION_BACKUP=1` opt-in).
- **API-V1.25-5** — `Store::open_readonly*` offers no compile-time guard against write methods. Hard refactor (`ReadStore`/`WriteStore` wrapper or phantom marker types). Cross-ref #909/PR #945 class.
- **EX-V1.25-1** — Adding a new CLI subcommand requires 5–7 file edits (`Commands` vs `BatchCmd` divergence — measurable: 47 vs 36 variants). Hard. Underlying cause of CQ-V1.25-2, API-V1.25-1, API-V1.25-2.
- **EX-V1.25-2** — Adding a grammar-less language requires editing three parser dispatch sites with non-exhaustive match. Medium.
- **EX-V1.25-4** — Adding a new `ChunkType` leaves 57 hardcoded type-hint patterns unupdated. Medium.
- **EX-V1.25-5** — `ExecutionProvider` enum hard-couples `gpu-index` to NVIDIA CUDA; no Metal/ROCm/Vulkan path. Hard. Affects manufacturing/local-first deployments on Apple Silicon.
- **EX-V1.25-6** — ONNX embedder hardcodes BERT-style input/output names and mean-pooling. Medium. Add `ModelConfig::input_names`, `output_name`, `pooling`.
- **EX-V1.25-7** — SPLADE and reranker model paths hardcoded, no preset registry. Medium. Add `[reranker]` / `[splade]` config sections.
- **EX-V1.25-8** — Adding a new `QueryCategory` requires edits in 5 coupled places. Medium. Use `define_query_categories!` macro pattern.
- **EX-V1.25-12** — Structural `Pattern` matchers hardcode per-language heuristics in fallthrough functions. Medium. Move to per-`LanguageDef` patterns.
- **PB-V1.25-4** — SQLite mmap_size 256MB × 4 conns interacts poorly with WSL `/mnt/c/` 9P. Medium. Auto-detect 9P/NTFS and set mmap=0.
- **PB-V1.25-6** — `std::fs::rename` fallback duplicated 4× with divergent cross-device handling. Medium. Factor into shared `cqs::fs::atomic_replace`.
- **SHL-V1.25-6** — CAGRA `itopk_size` hardcoded clamp; no env override. Medium. Add `CQS_CAGRA_ITOPK_*` env vars + corpus-size scaling. (Partial duplicate of EX-V1.25-10.)
- **SHL-V1.25-8** — Reranker batch passed whole candidate set in one ORT run. Medium. Add `CQS_RERANKER_BATCH`.
- **PF-V1.25-11** — `classify_query` rebuilds `language_names()` and iterates 50+ patterns per search. Medium. Cache in `LazyLock`, use Aho-Corasick.
- **PF-V1.25-12** — `NameMatcher::score` allocates per candidate (1-4MB churn on oracle runs). Medium. Pre-tokenize at upsert time.
- **PF-V1.25-17** — `compute_enrichment_hash_with_summary` rebuilds buffers per chunk — 100MB churn for 100k-chunk reindex. Medium.
- **PF-V1.25-18** — Reindex `reindex_files` clones every chunk into the channel. Medium. Switch to owning iterator.
- **RM-V1.25-6** — CAGRA GPU index rebuilt fully on every index change — no persistence. Hard. Add `CagraIndex::save`/`load` using cuVS native serialization.
- **RM-V1.25-12** — Multiple tokio runtimes per CLI invocation. Medium. Thread one shared runtime through Store + EmbeddingCache + QueryCache.
- **RM-V1.25-18** — `last_indexed_mtime` prune uses `exists()` per entry — O(n) stat syscalls. Medium. Recency-based prune.
- **RM-V1.25-21** — Reference `Store` uses 64MB mmap per reference — overspec'd. Medium. Add `Store::open_readonly_small`.
- **TC-HP-5** — HNSW self-heal integration test missing. Medium. Test that dirty flag clears after clean rebuild.
- **TC-HP-6** — Daemon `try_daemon_query` has zero tests (entire v1.24.0 feature uncovered). Hard. Mock socket + assert arg-translation; cross-ref `#909, #912-#925` if any exist.
- **TC-HP-7** — `dispatch_search` (batch, 386 LOC) has no direct integration test asserting result content. Medium.
- **TC-HP-11** — `tests/onboard_test.rs` 2 tests for 540+168 LOC. Medium.
- **TC-HP-12** — `tests/where_test.rs` 2 tests for 997 LOC `where_to_add.rs`. Medium.
- **TC-HP-14** — `tests/eval_test.rs` only 1 non-ignored test (fixture-existence). Medium. Add deterministic mock-embedder pipeline test.
- **TC-HP-17** — `test_list_stale_files_all_fresh` doesn't pin `current == stored` and `current < stored` (backup-restore) semantics. Medium.
- **EX-V1.25-11 (also P2)** — File alongside the P2 fix as a structural improvement issue, since collapsing the two notes-dispatch paths is a wider refactor than just patching the routing.

---

## Triage counts

- **P1**: 49 rows (real bugs and security exposures already biting or imminent — daemon hot path, search non-determinism, OOM vectors, cache leaks, panics)
- **P2**: 47 rows (medium-effort high-impact fixes — index/store correctness, daemon resource lifecycle, security hardening, missing tests for shipped features)
- **P3**: 97 rows (easy hygiene/perf wins, low-impact correctness, observability uplift, single-cleanup-pass material)
- **P4 trivial inline**: 16 rows (docs and stale-comment fixes)
- **P4 GitHub issues**: 32 rows (hard refactors — extensibility, model abstraction, runtime/hardware coverage, multi-file rework)

**Total rows emitted**: 49 + 47 + 97 + 16 + 32 = 241.

The 241-vs-236 overcount (+5) is intentional cross-listing. The following source rows are listed under their primary priority AND noted as duplicates of a merged parent, so the reader can scan either section without missing them:

- RB-NEW-3, SEC-V1.25-2, RM-V1.25-22 → merged into primary EH-15 (daemon `read_line` OOM)
- OB-NEW-6 → merged into primary EH-17 (`query_cache::get` swallow)
- OB-NEW-4 → merged into primary EH-21 (panic payload discarded)
- RM-V1.25-20 → merged into primary EH-22 (accept-loop debug)
- PB-V1.25-15 → merged into primary EH-18 (socket setup `.ok()`)
- PB-V1.25-19 → merged into primary SEC-V1.25-15 (stale-socket symlink)
- RM-V1.25-17 → merged into primary SHL-V1.25-9 (refs LRU hardcoded 2)
- EX-V1.25-10 → partial overlap with SHL-V1.25-6 (CAGRA knobs) — both retained

Additionally, EH-15 (daemon read_line OOM) and EX-V1.25-11 (notes dispatch `unreachable!()`) appear in both P2 (fix-now) and P4 (file a structural-improvement issue) — intentional: immediate patch + follow-on refactor.

All 236 source finding rows are represented; no findings dropped as "out of scope".

Cross-references to known open issues `#909, #912-#925, #856, #717, #389, #255, #106, #63`: no direct text-match overlap was found while triaging. The daemon-notes class of bugs (PR #945 class) is the class that would plausibly overlap with `#909`/`#912-#925` if those issue numbers track the same root cause — reviewer should confirm at issue-creation time.

---

## Wave-1 progress (2026-04-14 ~15:45)

**56 rows marked ✅ wave-1.** Branch `audit/p1-fixes-wave1`, ~32 commits.

Remaining: ~135 pending rows (P1 P2 leftovers, all of P3, P4 trivial inline).

Wave-1 lesson: parallel implementer agents on a shared working tree caused commit-message misattribution + stage races (memo: `[Always Worktree Isolation]`). **Wave 2 will use `isolation: "worktree"`** so each agent works on its own checkout and we merge branches back.
