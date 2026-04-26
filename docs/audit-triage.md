# v1.30.0 Post-Release Audit Triage

Generated: 2026-04-26T20:42:41Z
Total findings: 170 (8 batch 1 categories + 8 batch 2 categories)

Classification: P1 (easy + high impact, fix immediately) · P2 (medium + high, batch) · P3 (easy + low, quick wins) · P4 (hard or low impact, issues or inline trivial)

## P1 — Fix Immediately

| # | Title | Category | Location | Status |
|---|-------|----------|----------|--------|
| P1.1 | PRIVACY/SECURITY claim query_log is opt-in but it's unconditional | Documentation | `PRIVACY.md:22, SECURITY.md:101` vs `src/cli/batch/commands.rs:371` | ✅ fixed |
| P1.2 | PRIVACY claims 7-day TTL on query_cache.db; only size cap exists | Documentation | `PRIVACY.md:21` vs `src/cache.rs:1536` | ✅ fixed |
| P1.3 | CHANGELOG names CQS_LLM_ENDPOINT — actual var is CQS_LLM_API_BASE | Documentation | `CHANGELOG.md:19` | ✅ fixed |
| P1.4 | CONTRIBUTING tells contributors to edit dispatch.rs (registry.rs now) | Documentation | `CONTRIBUTING.md:339-355` | ✅ fixed |
| P1.5 | ProjectRegistry doc lies about path on macOS/Windows | Platform | `src/project.rs:1-3, 176` | ✅ fixed |
| P1.6 | gather warning hardcodes "200" — lies when CQS_GATHER_MAX_NODES set | Code Quality | `src/cli/commands/search/gather.rs:200` | ✅ fixed |
| P1.7 | Reranker silently ignores [reranker] config section | Code Quality | `src/reranker.rs:127-154, 442` | ✅ fixed |
| P1.8 | Embedder fingerprint falls back to repo:timestamp — cache thrash | Error Handling | `src/embedder/mod.rs:435-466` | ✅ fixed |
| P1.9 | LocalProvider Mutex::into_inner().unwrap_or_default() loses all batch results on poison | Error Handling | `src/llm/local.rs:155, 196, 271-279, 305` | ✅ fixed |
| P1.10 | LocalProvider unbounded HTTP body read — OOM on hostile/buggy server | Robustness | `src/llm/local.rs:97-100, 474-487` | ✅ fixed |
| P1.11 | Auth token leaked into TraceLayer span URI logging | Security | `src/serve/mod.rs:195` + `src/serve/auth.rs:226` | ✅ fixed |
| P1.12 | enforce_host_allowlist passes through missing Host header — DNS-rebinding bypass | Security | `src/serve/mod.rs:234-251` | ✅ fixed |
| P1.13 | Auth token printed to stdout — captured by journald for 30-day retention | Security | `src/serve/mod.rs:111-117` | ✅ fixed |
| P1.14 | cqs serve has no RequestBodyLimitLayer — authenticated client can OOM via large POST | Security | `src/serve/mod.rs:154-196` | ✅ fixed |
| P1.15 | UMAP coords not invalidated on chunk content change — cluster view serves stale | Data Safety | `src/store/chunks/async_helpers.rs:339` | ✅ fixed |
| P1.16 | --name-boost CLI accepts >1 / <0 — embedding signal sign-flips, deletes good results | Algorithm | `src/cli/args.rs:57` + `src/search/scoring/candidate.rs:286` | ✅ fixed (consumer-side clamp; CLI parser fix already shipped in v1.29.x) |
| P1.17 | drain_pending_rebuild dedup drops fresh embeddings during rebuild window | Algorithm | `src/cli/watch.rs:1077-1105` | pending |
| P1.18 | token_pack break-on-first-oversized — drops smaller items that would fit | Algorithm | `src/cli/commands/mod.rs:398-417` | ✅ fixed |
| P1.19 | cqs serve shutdown handles only Ctrl-C — SIGTERM (systemctl) skips graceful drain | Platform | `src/serve/mod.rs:253-260` | ✅ fixed |
| P1.20 | OB-V1.30-1 default subscriber drops every info_span — 150 spans invisible at default level | Observability | `src/main.rs:14-32` | ✅ fixed |
| P1.21 | Auth failures log nothing — no journal trail for 401s | Observability | `src/serve/auth.rs:194-232` | ✅ fixed |

## P2 — Batch Fix

| # | Title | Category | Location | Status |
|---|-------|----------|----------|--------|
| P2.1 | cmd_similar JSON emits 3 fields vs CLI's 9 — schema parity drop | Code Quality | `src/cli/batch/handlers/info.rs:139` | pending |
| P2.2 | dispatch_diff target_store placeholder dead in else branch | Code Quality | `src/cli/batch/handlers/misc.rs:354-387` | pending |
| P2.3 | Embedding/Query cache open_with_runtime ~80% copy-paste (90+ lines) | Code Quality | `src/cache.rs:103-220, 1412-1522` | pending |
| P2.4 | Repeated env::var parse pattern at 25+ sites — shared helpers exist but private | Code Quality | `src/limits.rs:230-260` + 25 sites | pending |
| P2.5 | EmbeddingCache accepts MAX_SIZE=0; QueryCache rejects — opposite behavior | Code Quality | `src/cache.rs:206-209, 1509-1513` | pending |
| P2.6 | DOC: README "544-query eval" should be 218; metrics also stale | Documentation | `README.md:5,649` | pending |
| P2.7 | DOC: README claims 54 languages but Cargo.toml/source disagree (Elm) | Documentation | `README.md:5,530-585` | pending |
| P2.8 | DOC: SECURITY omits per-project embeddings_cache.db | Documentation | `SECURITY.md:65-82` | pending |
| P2.9 | DOC: README/Claude integration list missing 5 commands (ping/eval/model/serve/refresh) | Documentation | `README.md:467-525` | pending |
| P2.10 | DOC: README cache subcommands list missing `clear` | Documentation | `README.md:521` | pending |
| P2.11 | --json model swap/show emits plain-text errors — no envelope | API Design | `src/cli/commands/infra/model.rs` | pending |
| P2.12 | cqs init/index/convert lack --json | API Design | `src/cli/commands/infra/init.rs`, etc. | pending |
| P2.13 | Global --slot silently ignored by `slot`/`cache` subcommands | API Design | `src/cli/definitions.rs` + slot/cache cmds | pending |
| P2.14 | cqs refresh has no --json | API Design | `src/cli/definitions.rs:755-761` | pending |
| P2.15 | List-shape JSON envelopes inconsistent across `*list` commands | API Design | `cmd_ref_list/cmd_model_list/cmd_project_list` | pending |
| P2.16 | cache stats mixes bytes and MB; cache compact uses bytes only | API Design | `src/cli/commands/infra/cache_cmd.rs` | pending |
| P2.17 | dispatch::try_daemon_query warns then silently re-runs in CLI | Error Handling | `src/cli/dispatch.rs:445-462` | pending |
| P2.18 | LocalProvider::fetch_batch_results returns empty map on missing batch_id | Error Handling | `src/llm/local.rs:542-547` | pending |
| P2.19 | impact/format `serde_json::to_value...unwrap_or_else(json!({}))` at 6 sites | Error Handling | `src/impact/format.rs`, etc. | pending |
| P2.20 | cache_stats silently treats QueryCache::open failure as 0 bytes | Error Handling | `src/cli/commands/infra/cache_cmd.rs:120-139` | pending |
| P2.21 | slot_remove masks list_slots failure as "only slot remaining" | Error Handling | `src/cli/commands/infra/slot.rs:303-313` | pending |
| P2.22 | build_token_pack swallows get_caller_counts_batch — silently degrades ranking | Error Handling | `src/cli/commands/io/context.rs:438-441` | pending |
| P2.23 | read --focus silently empties type_chunks on store batch failure | Error Handling | `src/cli/commands/io/read.rs:230-235` | pending |
| P2.24 | serve::build_chunk_detail collapses NULL signature/content to empty string | Error Handling | `src/serve/data.rs:488-492` | pending |
| P2.25 | Per-request span and build_* spans disconnected (spawn_blocking drops span ctx) | Observability | `src/serve/handlers.rs:86,111,131,160,210,236` | pending |
| P2.26 | TC-ADV: LocalProvider body-size DoS — buffers entire HTTP response | Test Coverage (adv) | `src/llm/local.rs:474-500` | pending |
| P2.27 | TC-ADV: EmbeddingCache/QueryCache accept NaN/Inf — cross-process cache poisoning | Test Coverage (adv) | `src/cache.rs:332-407, 1677-1699` | pending |
| P2.28 | TC-ADV: slot_create/slot_remove TOCTOU under concurrent operation | Test Coverage (adv) | `src/cli/commands/infra/slot.rs:219-350` | pending |
| P2.29 | TC-ADV: Non-blocking HNSW rebuild — no panic/dim-drift/store-fail tests | Test Coverage (adv) | `src/cli/watch.rs:965-1042` | pending |
| P2.30 | TC-ADV: serve auth strip_token_param case/percent-encoding gaps | Test Coverage (adv) | `src/serve/auth.rs:101-115` | pending |
| P2.31 | TC-ADV: slot::migrate_legacy rollback path untested; rollback failure leaves split state | Test Coverage (adv) | `src/slot/mod.rs:511-593` | pending |
| P2.32 | TC-ADV: LocalProvider non-HTTP api_base + concurrency mis-sizing | Test Coverage (adv) | `src/llm/local.rs:88-121` | pending |
| P2.33 | RB: Slot pointer files read with unbounded read_to_string | Robustness | `src/slot/mod.rs:207, 323` | pending |
| P2.34 | RB: migrate_legacy rollback leaves undetectable half-state | Robustness | `src/slot/mod.rs:511-593` | pending |
| P2.35 | RB: local.rs auth_attempts mutex unwrap cascades worker poison | Robustness | `src/llm/local.rs:393-396` | pending |
| P2.36 | RB: redirect policy disagrees between production (none) and doctor (limited(2)) | Robustness | `src/llm/local.rs:99` vs `src/cli/commands/infra/doctor.rs:578` | pending |
| P2.37 | SHL: CAGRA itopk_size < k on small indexes — silent zero-result regression | Scaling | `src/cagra.rs:359` | pending |
| P2.38 | SHL: nl::generate_nl char_budget defaults to 512 even with 2048 max_seq_len | Scaling | `src/nl/mod.rs:222-229` | pending |
| P2.39 | SHL: MAX_BATCH_SIZE=10_000 silently truncates summary/HyDE on large corpora | Scaling | `src/llm/mod.rs:192` | pending |
| P2.40 | SHL: serve graph/cluster cap 50_000 hardcoded; chunk_detail LIMIT 50/50/20 | Scaling | `src/serve/data.rs:17,24,505,542,571` | pending |
| P2.41 | SHL: embed_batch_size default 64 doesn't scale with model dim/seq | Scaling | `src/cli/pipeline/types.rs:143` | pending |
| P2.42 | SHL: CagraIndex::gpu_available has no VRAM ceiling — OOMs on 8GB GPUs | Scaling | `src/cagra.rs:262-264` | pending |
| P2.43 | semantic_diff sort lacks tie-breaker — non-deterministic JSON across runs | Algorithm | `src/diff.rs:202-207` | pending |
| P2.44 | is_structural_query keyword probe misses keywords at end-of-query | Algorithm | `src/search/router.rs:787-789` | pending |
| P2.45 | bfs_expand processes seeds in HashMap order — non-deterministic name_scores at cap | Algorithm | `src/gather.rs:317-320` | pending |
| P2.46 | llm summary contrastive_neighbors top-K sort lacks tie-breaker | Algorithm | `src/llm/summary.rs:263-267` | pending |
| P2.47 | reranker compute_scores unchecked batch_size*stride; negative shape[1] panic | Algorithm | `src/reranker.rs:368-387` | pending |
| P2.48 | doc_comments select_uncached sort lacks chunk-id tertiary key | Algorithm | `src/llm/doc_comments.rs:222-242` | pending |
| P2.49 | map_hunks_to_functions returns hunks in HashMap order — non-deterministic impact-diff | Algorithm | `src/impact/diff.rs:38-168` | pending |
| P2.50 | search_reference threshold/weight ordering bug — under-samples corpus when weight<1 | Algorithm | `src/reference.rs:231-285` | pending |
| P2.51 | find_type_overlap chunk_info uses HashMap iteration — non-deterministic file attribution | Algorithm | `src/related.rs:131-157` | pending |
| P2.52 | CAGRA search_with_filter under-fills when included<k — caller can't distinguish | Algorithm | `src/cagra.rs:520-598` | pending |
| P2.53 | Hybrid SPLADE alpha=0 emits 1.0+s scores; cliff at SPLADE boundary | Algorithm | `src/search/query.rs:649-672` | pending |
| P2.54 | apply_scoring_pipeline sign-flips on out-of-range name_boost; clamp embedding pre-blend | Algorithm | `src/search/scoring/candidate.rs:283-298` | pending |
| P2.55 | open_browser uses explorer.exe on Windows — drops query string/token | Platform | `src/cli/commands/serve.rs:89-104` | pending |
| P2.56 | NTFS/FAT32 mtime equality check — watch loop skips second save on FAT32 USB | Platform | `src/cli/watch.rs:551-560` | pending |
| P2.57 | enforce_host_allowlist accepts missing Host header (dev ergonomic) | Platform | `src/serve/mod.rs:230-251` | pending |
| P2.58 | --bind 0.0.0.0 host-allowlist breaks LAN — pushes operators to --no-auth | Security | `src/serve/mod.rs:207-218` | pending |
| P2.59 | Migration restore_from_backup overwrites live DB while pool open | Data Safety | `src/store/backup.rs:171-180` | pending |
| P2.60 | stream_summary_writer bypasses WRITE_LOCK — concurrent writer collides with reindex | Data Safety | `src/store/chunks/crud.rs:504-545` | pending |
| P2.61 | slot_remove TOCTOU on concurrent promote — active_slot points to deleted dir | Data Safety | `src/cli/commands/infra/slot.rs:299-350` | pending |
| P2.62 | Slot legacy migration moves live WAL/SHM instead of checkpointing first | Data Safety | `src/slot/mod.rs:511-624` | pending |
| P2.63 | model_fingerprint fallback uses Unix timestamp — every restart misses cache | Data Safety | `src/embedder/mod.rs:435-465` | pending |
| P2.64 | Daemon serializes ALL queries through one Mutex<BatchContext> | Data Safety | `src/cli/watch.rs:1775-1858` | pending |
| P2.65 | embedding_cache schema doesn't separate `embedding` vs `embedding_base` purpose | Data Safety | `src/cache.rs:159-171` | pending |
| P2.66 | cache evict() vs write_batch race — evict deletes rows just inserted | Data Safety | `src/cache.rs:354-460` | pending |
| P2.67 | PF: reindex_files watch path double-parses calls per chunk | Performance | `src/cli/watch.rs:2815, 2930-2939` | pending |
| P2.68 | PF: reindex_files watch path bypasses global EmbeddingCache | Performance | `src/cli/watch.rs:2876-2887` | pending |
| P2.69 | PF: wrap_value deep-clones entire payload via serde round trip | Performance | `src/cli/json_envelope.rs:160-176` | pending |
| P2.70 | PF: build_graph correlated subquery for n_callers — N rows × COUNT(*) | Performance | `src/serve/data.rs:234-264` | pending |
| P2.71 | RM: Background HNSW rebuild thread detached — daemon shutdown can't wait | Resource Mgmt | `src/cli/watch.rs:965-1042` | pending |
| P2.72 | RM: pending_rebuild.delta grows unbounded during long rebuild | Resource Mgmt | `src/cli/watch.rs:611, 2667-2741` | pending |
| P2.73 | RM: LocalProvider::stash retains all submitted batch results until drop | Resource Mgmt | `src/llm/local.rs:74, 304-309, 542-547` | pending |
| P2.74 | RM: Daemon never checks fs.inotify.max_user_watches — silently drops events | Resource Mgmt | `src/cli/watch.rs:1947-1949` | pending |
| P2.75 | RM: select_provider triggers CUDA probe + symlink for every CLI process | Resource Mgmt | `src/embedder/provider.rs:171-248` | pending |
| P2.76 | RM: serve handlers spawn_blocking unbounded — 512 thread × 10MB working set | Resource Mgmt | `src/serve/handlers.rs:86-89` + `mod.rs:92` | pending |
| P2.77 | RM: Embedder clear_session doubled-memory window invisible | Resource Mgmt | `src/embedder/mod.rs:261, 808-823` | pending |
| P2.78 | TC-HAP: cqs serve data endpoints never tested with populated data | Test Coverage | `src/serve/data.rs` + `src/serve/tests.rs` | pending |
| P2.79 | TC-HAP: 16 batch dispatch handlers have zero tests | Test Coverage | `src/cli/batch/handlers/{misc,graph,info}.rs` | pending |
| P2.80 | TC-HAP: Reranker::rerank/rerank_with_passages have no tests | Test Coverage | `src/reranker.rs:160, 190` | pending |
| P2.81 | TC-HAP: cmd_project Search has no CLI integration test | Test Coverage | `src/cli/commands/infra/project.rs:70` | pending |
| P2.82 | TC-HAP: cqs ref add/list/remove/update no end-to-end CLI test | Test Coverage | `src/cli/commands/infra/reference.rs` | pending |
| P2.83 | TC-HAP: handle_socket_client no happy-path round-trip test | Test Coverage | `src/cli/watch.rs:160` | pending |
| P2.84 | TC-HAP: spawn_hnsw_rebuild/drain_pending_rebuild ship with zero tests | Test Coverage | `src/cli/watch.rs spawn_hnsw_rebuild` | pending |
| P2.85 | TC-HAP: for_each_command! macro + 4 emitters have no behavioral tests | Test Coverage | `src/cli/registry.rs:61` | pending |
| P2.86 | TC-HAP: build_hnsw_index_owned/build_hnsw_base_index — no direct tests | Test Coverage | `src/cli/commands/index/build.rs:848,880` | pending |
| P2.87 | TC-HAP: hyde_query_pass and doc_comment_pass have zero tests | Test Coverage | `src/llm/hyde.rs:11`, `src/llm/doc_comments.rs:135` | pending |
| P2.88 | EX: Adding third score signal touches two parallel fusion paths | Extensibility | `src/store/search.rs:182-229`, `src/search/query.rs:511-720` | pending |
| P2.89 | EX: Vector index backend selection is hand-coded if/else; no IndexBackend trait | Extensibility | `src/cli/store.rs:423-540` | pending |
| P2.90 | EX: ScoringOverrides knob → 4 sites; no shared resolver | Extensibility | `src/config.rs:153-172` + scoring | pending |
| P2.91 | EX: NoteEntry has no kind/tag taxonomy — only sentiment | Extensibility | `src/note.rs:41-89` | pending |
| P2.92 | RM: Embedder::new opens fresh QueryCache + 7-day prune on every CLI command | Resource Mgmt | `src/embedder/mod.rs:355-366` | pending |

## P3 — Quick Wins

| # | Title | Category | Location | Status |
|---|-------|----------|----------|--------|
| P3.1 | panic_message helper duplicated 4 ways across 3 modules | Code Quality | `src/cli/pipeline/mod.rs:223`, `src/store/mod.rs:1322`, etc. | pending |
| P3.2 | resolve.rs find_reference + resolve_reference_db duplicate "find by name" twice | Code Quality | `src/cli/commands/resolve.rs:26-57` | pending |
| P3.3 | slot::libc_exdev hardcodes 18 with stale comment — libc is workspace dep | Code Quality | `src/slot/mod.rs:640-647` | pending |
| P3.4 | DOC: enumerate_files doc claims gitignore only — also honors .cqsignore | Documentation | `src/lib.rs:542-547` | pending |
| P3.5 | pub use nl::* leaks dead generate_nl_with_call_context wrapper | API Design | `src/lib.rs:165` + `src/nl/mod.rs:43-59` | pending |
| P3.6 | cqs gather --expand vs --expand-parent flag-name collision | API Design | `src/cli/args.rs:GatherArgs::expand` | pending |
| P3.7 | cqs eval --save accepts path with no .json validation | API Design | `src/cli/commands/eval/mod.rs:EvalCmdArgs::save` | pending |
| P3.8 | OB: cqs eval runner uses eprintln! for progress instead of tracing | Observability | `src/cli/commands/eval/runner.rs:163-168` | pending |
| P3.9 | OB: nl/mod.rs public NL generators have zero spans | Observability | `src/nl/mod.rs:43,65,189,209` | pending |
| P3.10 | OB: embed_documents/embed_query lack completion fields (result.len, dim, time) | Observability | `src/embedder/mod.rs:683,722` | pending |
| P3.11 | OB: Reranker::rerank_with_passages swallows length mismatch silently | Observability | `src/reranker.rs:200-220` | pending |
| P3.12 | OB: train_data git wrappers don't log non-zero exit codes | Observability | `src/train_data/git.rs:65-242` | pending |
| P3.13 | OB: format-string-interpolated tracing::info! at 9 sites — fields lost | Observability | `src/hnsw/build.rs:78,236` + 7 sites | pending |
| P3.14 | OB: cluster_2d emits no warn when corpus has chunks but zero UMAP rows | Observability | `src/serve/data.rs:901, 1020` | pending |
| P3.15 | TC-ADV: validate_slot_name accepts leading-dash / trailing-dash names | Test Coverage (adv) | `src/slot/mod.rs:159-178` | pending |
| P3.16 | TC-ADV: provider.rs ort_runtime_search_dir untested for malformed cmdline | Test Coverage (adv) | `src/embedder/provider.rs:67-123` | pending |
| P3.17 | TC-ADV: blake3_hex_or_passthrough uppercase/short-hex edges untested | Test Coverage (adv) | `src/cache.rs:709-721` | pending |
| P3.18 | RB: SystemTime → i64 cache cast wraps in 2554 | Robustness | `src/cache.rs:349-352, 551-555` | pending |
| P3.19 | RB: libc_exdev hardcodes 18 — wrong on Windows (ERROR_NOT_SAME_DEVICE=17) | Robustness | `src/slot/mod.rs:644-647` | pending |
| P3.20 | RB: cache prune --older-than DAYS computes negative cutoff for huge values | Robustness | `src/cache.rs:548, 551-555` | pending |
| P3.21 | RB: serve/data.rs i64.max(0) as u32 grew to 8 sites (was 3) | Robustness | `src/serve/data.rs` (8 sites) | pending |
| P3.22 | RB: Daemon socket-thread join detaches on timeout but logs "joined cleanly" | Robustness | `src/cli/watch.rs:2374-2400` | pending |
| P3.23 | SHL: diff EMBEDDING_BATCH_SIZE=1000 doesn't scale with model dim | Scaling | `src/diff.rs:158` | pending |
| P3.24 | SHL: Daemon worker_threads=min(num_cpus,4) hardcoded — caps large machines | Scaling | `src/cli/watch.rs:115-119` | pending |
| P3.25 | SHL: train_data MAX_SHOW_SIZE=50MB hardcoded — silent skip on big files | Scaling | `src/train_data/git.rs:167` | pending |
| P3.26 | EX: BatchCmd::is_pipeable is a separate match outside command registry | Extensibility | `src/cli/batch/commands.rs:325-538` | pending |
| P3.27 | EX: LlmProvider resolver hand-codes 2 providers — no registry | Extensibility | `src/llm/mod.rs:200-398` | pending |
| P3.28 | EX: Tree-sitter query files no startup self-test (registry consistency) | Extensibility | `src/language/queries/*.scm` | pending |
| P3.29 | EX: find_project_root markers list hardcoded — could be data | Extensibility | `src/cli/config.rs:155-162` | pending |
| P3.30 | EX: structural_matchers per-language fn — no shared library | Extensibility | `src/language/mod.rs:191,345` | pending |
| P3.31 | EX: Embedder constructor no per-preset extras hook | Extensibility | `src/embedder/models.rs:163-300` | pending |
| P3.32 | PB: EmbeddingCache/QueryCache hardcode ~/.cache/cqs on Windows | Platform | `src/cache.rs:80-84, 1399-1403` | pending |
| P3.33 | PB: dispatch_drift/diff JSON file fields use display() in suggest.rs/types.rs | Platform | `src/suggest.rs:101`, `src/store/types.rs:220` | pending |
| P3.34 | PB: find_ld_library_dir splits on `:` — no Windows arm | Platform | `src/embedder/provider.rs:115-123` | pending |
| P3.35 | PB: index.lock advisory on Linux but mandatory on Windows; doc gap | Platform | `src/cli/files.rs:120-213` | pending |
| P3.36 | PB: is_wsl_drvfs_path misses //wsl.localhost and uppercase mounts | Platform | `src/config.rs:92-101` | pending |
| P3.37 | PB: blame git_file = replace('\\', "/") — Windows verbatim prefix slips through | Platform | `src/cli/commands/io/blame.rs:113-115` | pending |
| P3.38 | PB: daemon_socket_path falls back to temp_dir silently — log differing trust | Platform | `src/daemon_translate.rs:179-188` | pending |
| P3.39 | DS: write_slot_model/write_active_slot skip parent-dir fsync after rename | Data Safety | `src/slot/mod.rs:237-406` | pending |
| P3.40 | DS: update_umap_coords_batch uses TEMP TABLE shared across calls | Data Safety | `src/store/chunks/crud.rs:392-450` | pending |
| P3.41 | PF: reindex_files allocates N empty Embedding placeholders | Performance | `src/cli/watch.rs:2918-2924` | pending |
| P3.42 | PF: prepare_for_embedding always issues store-cache query even on full global hit | Performance | `src/cli/pipeline/embedding.rs:64-82` | pending |
| P3.43 | PF: Daemon socket walks args array twice (validation + extraction) | Performance | `src/cli/watch.rs:266-297` | pending |
| P3.44 | PF: build_graph edge-dedup HashSet keys clone (file,caller,callee) per row | Performance | `src/serve/data.rs:367-373` | pending |
| P3.45 | PF: extract_imports HashSet<String> allocates per candidate even on duplicate | Performance | `src/where_to_add.rs:258-276` | pending |
| P3.46 | PF: Watch reindex cached embedding clone via .get instead of .remove | Performance | `src/cli/watch.rs:2879-2887` | pending |
| P3.47 | RM: LocalProvider worker threads use default 2MB stack — 128MB at concurrency=64 | Resource Mgmt | `src/llm/local.rs:163-256` | pending |
| P3.48 | RM: LocalProvider::http no pool_max_idle / idle_timeout | Resource Mgmt | `src/llm/local.rs:97-100` | pending |
| P3.49 | TC-HAP: cmd_similar (CLI) has no integration test | Test Coverage | `src/cli/commands/search/similar.rs:41` | pending |
| P3.50 | TC-HAP: cmd_ci happy path untested; only error paths tested | Test Coverage | `src/cli/commands/review/ci.rs:9` | pending |
| P3.51 | TC-HAP: cmd_gather (CLI) untested; only library gather() tested | Test Coverage | `src/cli/commands/search/gather.rs:77` | pending |
| P3.52 | TC-HAP: dispatch_line no happy-path test for valid command | Test Coverage | `src/cli/batch/mod.rs:557` | pending |
| P3.53 | TC-HAP: select_provider/detect_provider untested (#1120 split) | Test Coverage | `src/embedder/provider.rs:171-258` | pending |

## P4 — Defer / Issues

| # | Title | Category | Location | Disposition | Status |
|---|-------|----------|----------|-------------|--------|
| P4.1 | AuthToken::from_string cfg-gated, alphabet invariant relies on docstring | Security | `src/serve/auth.rs:75-78, 218` | issue (hardening) | pending |
| P4.2 | Path=/ cookie scope on 127.0.0.1 — multiple cqs serve on same host stomp | Security | `src/serve/auth.rs:211-214` | issue (browser cookie limit) | pending |
| P4.3 | Auth state ignored by quiet=true — Option<AuthToken> permits silent no-auth | Security | `src/serve/mod.rs:78-83` | issue (type-state refactor) | pending |

## Summary

- **P1: 21 findings** — biggest themes:
  - Docs lying about security/privacy (query_log opt-in, query_cache TTL, registry/dispatch contributor docs, project registry path)
  - Auth token leakage (TraceLayer span URI, journald banner, missing-Host bypass)
  - Critical defaults misleading users (gather "200" warning, reranker config ignored, embedder fingerprint cache thrash)
  - Single-line wiring bugs (LocalProvider mutex poison loses results, name_boost sign-flip, token_pack break-vs-continue, drain_pending_rebuild dedup)
  - Observability "off" by default — 150 spans invisible until OB-V1.30-1 lands
- **P2: 92 findings** — biggest themes:
  - Determinism / tie-break gaps in 8+ sort sites (semantic_diff, bfs_expand, contrastive_neighbors, doc_comments, map_hunks, related, etc.)
  - Cross-slot data-safety issues stemming from #1105 (slot TOCTOU, fingerprint fallback, evict/write race, schema purpose conflation)
  - Untested v1.30.0 surfaces (#1113 HNSW rebuild, #1114 registry, #1118 auth, #1120 provider split, serve data endpoints, batch dispatch handlers, LLM passes)
  - Config/JSON contract drift (--json absent on init/index/convert/refresh; list shapes inconsistent; cache stats unit mix)
  - Resource-management leaks introduced in v1.30.0 (detached rebuild thread, pending.delta unbounded, LocalProvider stash, eager QueryCache open, eager CUDA probe)
- **P3: 53 findings** — biggest themes:
  - Observability convention drift (eprintln, format-string interpolation, missing completion fields, missing spans on hot fns)
  - Platform/Windows refinements (cache paths, path normalization, EXDEV constant, WSL UNC paths, mtime FAT32)
  - Performance micro-opts (allocator churn, unnecessary clone, double-pass scans, correlated subquery on small budget)
  - Adversarial test additions for newly-shipped surfaces
- **P4: 3 findings** — biggest themes:
  - Auth/security hardening that requires type-state refactors or cookie scope changes browser-wide
  - All three are tracking-issue material; no inline triv

## Cross-cutting Observations

- **Docs lying is concentrated in the v1.30.0 release surface.** PRIVACY/SECURITY/CHANGELOG/CONTRIBUTING/README each have ≥1 P1 lie (query_log opt-in, query_cache TTL, CQS_LLM_ENDPOINT, dispatch.rs procedure, project registry path). Each is an easy text fix; together they indicate the v1.30.0 release notes pass (#1122) didn't cross-check against actual code paths.
- **Mutex/error-handling silent-failure pattern recurs across LocalProvider (#1101).** RB-V1.30-1 (unbounded body), RB-V1.30-7 (auth_attempts mutex unwrap), TC-ADV-1.30-1 (body DoS), EH (Mutex::into_inner unwrap_or_default loses batch results), and the silent fetch_batch_results empty-on-missing all stem from the same "build this fast for #1101 ship date" pattern. A focused PR to harden LocalProvider would close 5+ findings.
- **#1105 (slots+cache) introduced 8+ TOCTOU/race/cache-mismatch findings.** slot_remove vs slot_promote, slot migrate rollback, embedding_cache purpose column missing, model_fingerprint timestamp fallback breaking cross-slot copy, evict-vs-write race, EmbeddingCache vs QueryCache zero-handling divergence, cache stats silent-zero. The cache+slots subsystem is overdue for a hardening pass with locks (`.cqs/slots.lock`) and schema purposes.
- **Determinism regressions cluster around HashMap iteration in algorithm code.** semantic_diff sort, bfs_expand seed enqueue, contrastive_neighbors top-K, doc_comments select, map_hunks_to_functions, find_type_overlap chunk_info — six findings, all the same root cause (HashMap iter into score-sorted result), and all easy fixes. One sweep PR closes them.
- **The v1.30.0 critical surfaces shipped without tests.** #1113 (non-blocking HNSW rebuild), #1114 (single-registration registry), #1118 (serve auth — strip_token_param, missing-Host), #1120 (execution-provider split), serve data endpoints, 16 batch dispatch handlers — all under-tested. TC-HAP findings P2.78–P2.87 form a coherent test-debt PR series that would close ~10 findings and provide regression protection for the next release.
- **Observability "applied to new modules but default-off" pattern.** v0.12.1 lesson lifted spans into every new module, but OB-V1.30-1 reveals the default subscriber drops everything. Fixing the default plus structured-field cleanup (P3.13) and lazy span propagation across spawn_blocking (P2.25) would make the existing instrumentation actually useful in production.
