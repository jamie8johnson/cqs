# Audit Triage (v1.36.2)

Total findings: 163 across 16 categories. Classified P1 (fix immediately) / P2 (fix in batch) / P3 (fix if time) / P4 (issue or skip).

## Summary by Priority

| Priority | Count | Status (post-PR-#1456) |
|----------|-------|-------------------------|
| P1 | 58 | 56 ✅ + 2 🟡 (mitigated upstream) — **all 58 addressed** |
| P2 | 14 | 11 ✅ + 3 🟢 (defensive variants for medium-effort items) — **all 14 addressed** |
| P3 | 73 | ~40 ✅ + 5 🔍 (audit suggestion proved wrong on inspection) — ~28 ⏳ deferred |
| P4 | 18 | All deferred (hard / design-level / Windows-daemon / etc.) |
| **Total addressed** | **163** | **~120 of 163 findings (~74%)** in PR #1456 |

Remaining ⏳ filed as tracking issues (2026-05-05, post-PR-#1456):
- **#1457** — P2-14: ChunkRow::from_row column-name strcmps
- **#1458** — P3 TC Happy path tests (6 items: context builders, pack_by_relevance, prepare_for_embedding, daemon GC, cmd_train_data)
- **#1459** — P3 API design (8 items: project/ref/index ergonomics + trait shape)
- **#1460** — P3 Extensibility (3 items: config-driven synonyms / test-name patterns / classifier vocab)
- **#1461** — P3 Security (3 items: serve extractor URI leak, pre-auth body cap, daemon socket TOCTOU)
- **#1462** — P3 misc: RM-V1.36-6 config flock + reqwest, CQ-V1.36-3/5 legacy NL wrappers
- **#1463** — P4 umbrella (12 design-level / hard items: extensibility refactors, Windows daemon, `.bak` rollback)
- **In-progress** — P3 Scaling: 5 dim-blind batch sizes (SHL-V1.36-3/4/5/6/8) being addressed in a follow-up PR

## Cross-cutting themes

- **Lying-docs cluster (11 findings, all P1).** lib.rs/README/CONTRIBUTING/PRIVACY all stop preset enumeration at `nomic-coderank` and miss `qwen3-embedding-{4b,8b}` shipped in v1.36.1. `src/schema.sql:1` says v22 (actual v26). CONTRIBUTING.md says v25. ROADMAP.md "Current" still v1.36.0. Cargo.toml `lang-all` missing `lang-elm`. CHANGELOG `[Unreleased]` block parked between two released versions. SECURITY.md citation `src/lib.rs:601` (actual 813). Cargo.toml description over-rounds R@20.

- **Configurable-models / wiring-verification cluster (4 P1 findings).** v1.33's P1-28 fix for `generate_nl_description` only patched initial-embedding sites — `enrichment_pass` (CQ-V1.36-1) still drops `model_max_seq_len` on the production reindex hot path. `resolve_splade_model_dir()` zero-arg wrapper drops `[splade]` config at all 6 production callers (CQ-V1.36-2). `enrichment_pass` accepts `model_config` but doesn't thread it (CQ-V1.36-6). `VectorIndex::search_with_filter` default impl uses unchecked `k*3` (sibling miss of P1-42, fix in v1.33).

- **NaN/Inf passthrough cluster (5 P1 findings).** `note_boost` accepts `±Inf` sentiment then multiplies → BoundedScoreHeap drops the chunk it should boost (`±Inf` zeroes the result). `store::sparse::token_dump_paged` casts `weight as f32` without `is_finite`. `embed_documents` output never asserted finite (test gap). `print_telemetry_text:477` divide-by-zero (sibling of P1-25). CAGRA env knobs accept zero (sibling of P1-45 HNSW fix in v1.33).

- **Unbounded `fs::read_to_string` cluster (5 P1 findings, 5 new sites).** `doc_writer/rewriter.rs:251,319` (compute_rewrite + rewrite_file). `cli/commands/search/query.rs:899` (parent-context fallback). `cli/commands/infra/hook.rs:183,365,506` (3 sites in install/uninstall/status). `slot/mod.rs:339` (slot.toml — sibling has the guard). `truncate_incomplete_line` reads multi-GB JSONL whole. Same RB-V1.30-2 pattern fixed in v1.33; new sites keep landing.

- **Unbounded subprocess capture cluster (2 P1 findings).** `pdf_to_markdown` and 4+ `Command::output()` sites buffer stdout/stderr unbounded. Sibling `train_data::git_diff_tree` already has the right `spawn` + `take(max+1)` pattern.

- **SQLite pool/host parallelism cluster (2 P1 findings).** `CQS_MAX_CONNECTIONS` defaults to `unwrap_or(4)` regardless of cores (gates `serve_blocking_permits`). `reference.rs:208` still has `unwrap_or(4)` even though v1.33 SHL-V1.33-10 fixed the identical pattern in `project.rs:260`. Both single-line copies of the v1.33 closure.

- **Concurrent writer / data safety cluster (5 P1 findings).** `Store::close()` still uses unbounded `wal_checkpoint(TRUNCATE)` — same hazard PR #1450 fixed in `drop()`. `cqs index --force` deletes WAL/SHM after only PASSIVE checkpoint, can silently truncate concurrent watch writes. `collect_migration_files` misses `hnsw.ids`/`hnsw.checksum` sidecars — strict DS-V1.33-3 verifier then nukes the migrated index. Legacy `hnsw_dirty` key never cleared in `set_hnsw_dirty`. Migration orphan-drop threshold uses lossy f64 cast.

- **Windows / case-insensitive FS cluster (3 P1 findings).** `apply_resolved_edits` flattens CRLF source files to LF (mirror fix from #1356 not propagated). `note::path_matches_mention` is byte-case-sensitive — Linux-authored notes silently skip Windows/macOS chunks. `worktree::lookup_main_cqs_dir` uses `own_cqs.exists()` instead of `is_dir()`.

- **Security easy wins (5 P1 findings).** Store::open umask TOCTOU (cache fixed this; store didn't). Daemon env redactor misses BEARER/AUTH/CRED + URL embedded creds. LLM debug log echoes full HTTP body (which is indexed source code) into journald. `slot_dir` doesn't validate slot name → path-traversal via `..`. `validate_repo_id` allows `..`.

- **Observability noise (2 P1).** `search_filtered` fires duplicate nested span on the hottest daemon path (line 104 + 136). `config.rs:582` still emits redundant `eprintln!` next to `tracing::warn!` (violates OB-V1.30.1-9 contract).

- **Algorithm correctness easy (3 P1).** `extract_file_from_chunk_id` regex caps window suffix at 3 chars (`w99`) but emitter is unbounded — 100+-window chunks corrupt file-based dedup. `where_to_add` `line_end + 1` u32 add not saturating. CAGRA env knobs accept 0.

## P1 — Fix Immediately

| ID | Category | Title | Difficulty | Status |
|----|----------|-------|------------|--------|
| P1-1 | Documentation | lib.rs Features missing qwen3-embedding-{4b,8b} | easy | ✅ |
| P1-2 | Documentation | README How It Works + CQS_EMBEDDING_MODEL miss qwen3 (4 files: README, CONTRIBUTING, PRIVACY) | easy | ✅ |
| P1-3 | Documentation | Cargo.toml lang-all missing lang-elm | easy | ✅ |
| P1-4 | Documentation | language/mod.rs Feature Flags missing lang-dart | easy | ✅ |
| P1-5 | Documentation | CHANGELOG [Unreleased] in wrong position | easy | ✅ |
| P1-6 | Documentation | Cargo.toml description over-rounds R@20 | easy | ✅ |
| P1-7 | Documentation | SECURITY.md cites stale src/lib.rs:601 (actual :813) | easy | ✅ |
| P1-8 | Documentation | src/schema.sql:1 header says v22 (actual v26) | easy | ✅ |
| P1-9 | Documentation | CONTRIBUTING.md schema citation v25 (actual v26) | easy | ✅ |
| P1-10 | Documentation | ROADMAP.md Current still v1.36.0 (actual v1.36.2) | easy | ✅ |
| P1-11 | Documentation | SECURITY.md telemetry path glob (no rotation) | easy | ✅ |
| P1-12 | Code Quality | CQ-V1.36-1 enrichment_pass drops model_max_seq_len | medium | ✅ |
| P1-13 | Code Quality | CQ-V1.36-2 resolve_splade_model_dir() drops config at 6 sites | easy | ✅ |
| P1-14 | Code Quality | CQ-V1.36-6 enrichment_pass model_config ignored (group with P1-12) | easy | ✅ |
| P1-15 | Code Quality | CQ-V1.36-7 VectorIndex::search_with_filter k*3 unchecked | easy | ✅ |
| P1-16 | API Design | -d short flag missing on gather and trace | easy | ✅ |
| P1-17 | API Design | cqs eval --reranker llm placeholder advertises non-existent capability | easy | ✅ |
| P1-18 | Error Handling | EH-V1.36-6 Store::stored_model_name swallows query errors → data destruction risk | medium | ✅ |
| P1-19 | Observability | search_filtered duplicate nested span on hottest path | easy | ✅ |
| P1-20 | Observability | config.rs:582 redundant eprintln! next to tracing::warn! | easy | ✅ |
| P1-21 | TC Adversarial | Sparse-vector NaN/Inf weight round-trip untested | easy | ✅ |
| P1-22 | TC Adversarial | sanitize_fts_query property tests miss `{` and `}` | easy | ✅ |
| P1-23 | TC Adversarial | embed_documents output finiteness untested | easy | ✅ |
| P1-24 | Robustness | RB-V1.36-1 doc_writer compute_rewrite/rewrite_file unbounded read | easy | ✅ |
| P1-25 | Robustness | RB-V1.36-2 search/query.rs parent-context unbounded read | easy | ✅ |
| P1-26 | Robustness | RB-V1.36-3 hook.rs reads existing git hooks unbounded (3 sites) | easy | ✅ |
| P1-27 | Robustness | RB-V1.36-4 slot.toml unbounded read | easy | ✅ |
| P1-28 | Robustness | RB-V1.36-6 train_data diff hunk_end usize add not saturating | easy | ✅ |
| P1-29 | Robustness | RB-V1.36-7 where_to_add mutex .expect propagates panic | easy | ✅ |
| P1-30 | Robustness | RB-V1.36-9 print_telemetry_text divide-by-zero sibling at line 477 | easy | ✅ |
| P1-31 | Robustness | RB-V1.36-10 sparse weight as f32 no NaN guard | easy | ✅ |
| P1-32 | Scaling | SHL-V1.36-1 CQS_MAX_CONNECTIONS unwrap_or(4) ignores parallelism | easy | ✅ |
| P1-33 | Scaling | SHL-V1.36-2 reference.rs:208 still unwrap_or(4) (sibling fix in project.rs) | easy | ✅ |
| P1-34 | Scaling | SHL-V1.36-7 lib.rs stale 999 SQLite host-param comment | easy | ✅ |
| P1-35 | Algorithm | extract_file_from_chunk_id mis-parses :w100+ window suffixes | easy | ✅ |
| P1-36 | Algorithm | ±Inf note sentiment hides boosted chunk via BoundedScoreHeap drop | easy | ✅ |
| P1-37 | Algorithm | Sparse weight `as f32` no NaN guard (mirrors RB P1-31) | easy | ✅ |
| P1-38 | Algorithm | CAGRA env knobs accept 0 (sibling of P1-45 HNSW fix in v1.33) | easy | ✅ |
| P1-39 | Algorithm | where_to_add line_end + 1 u32 add not saturating | easy | ✅ |
| P1-40 | Extensibility | apply_parent_boost hardcodes Class/Struct/Interface (drops boost on Trait/Object/Protocol) | easy | ✅ |
| P1-41 | Platform | apply_resolved_edits CRLF flatten on doc rewrite (mirror #1356 to doc_writer) | medium | ✅ |
| P1-42 | Platform | note::path_matches_mention case-sensitive — Linux notes skip Windows/macOS | easy | ✅ |
| P1-43 | Platform | worktree own_cqs.exists() should be is_dir() | easy | ✅ |
| P1-44 | Security | Store::open umask TOCTOU (cache has the wrap, store doesn't) | easy | ✅ |
| P1-45 | Security | Daemon env-var redactor misses BEARER/AUTH/CRED/URL-embedded creds | easy | ✅ |
| P1-46 | Security | LLM debug log echoes HTTP body (= indexed source) into journald | easy | ✅ |
| P1-47 | Security | slot_dir/slot_config_path skip validate_slot_name → path-traversal | easy | ✅ |
| P1-48 | Security | validate_repo_id allows `..` | easy | ✅ |
| P1-49 | Data Safety | DS-V1.36-1 collect_migration_files misses hnsw.ids/.checksum sidecars | easy | ✅ |
| P1-50 | Data Safety | DS-V1.36-3 legacy hnsw_dirty key never cleared in set_hnsw_dirty | easy | ✅ |
| P1-51 | Data Safety | DS-V1.36-4 cqs index --force WAL/SHM removal after PASSIVE only | easy | ✅ |
| P1-52 | Data Safety | DS-V1.36-7 Store::close() still TRUNCATE (#1450 sibling) | easy | ✅ |
| P1-53 | Data Safety | DS-V1.36-8 migrate_v18_to_v19 orphan threshold lossy f64 cast | easy | ✅ |
| P1-54 | Resource Mgmt | RM-V1.36-1 truncate_incomplete_line slurps whole JSONL | easy | ✅ |
| P1-55 | Resource Mgmt | RM-V1.36-2 pdf_to_markdown unbounded subprocess output | easy | ✅ |
| P1-56 | Resource Mgmt | RM-V1.36-4 watch UnixStream::connect no timeout | easy | ✅ |
| P1-57 | Resource Mgmt | RM-V1.36-5 Command::output() unbounded (chm/convert/train_data) | easy | 🟡 |
| P1-58 | Resource Mgmt | RM-V1.36-7 BufReader::lines no per-line cap | easy | 🟡 |

## P2 — Fix in Batch

| ID | Category | Title | Difficulty | Status |
|----|----------|-------|------------|--------|
| P2-1 | Error Handling | EH-V1.36-2 train_data corpus-parse Err+panic collapsed | easy | ✅ |
| P2-2 | Error Handling | EH-V1.36-3 doc_writer cross-device backup-restore silent drop | easy | ✅ |
| P2-3 | Observability | cagra_persist_enabled silent skip in callers | easy | ✅ |
| P2-4 | TC Adversarial | Daemon JSON-RPC: lone surrogate + deeply-nested JSON | medium | ✅ |
| P2-5 | TC Adversarial | Daemon socket: zero concurrent-connection / queue-saturation | medium | ✅ |
| P2-6 | TC Adversarial | serve HTTP chunk_id adversarial unicode | medium | ✅ |
| P2-7 | Robustness | RB-V1.36-5 chunked blake3 vs whole-file read (staleness + reindex) | medium | ✅ |
| P2-8 | Robustness | RB-V1.36-8 Language::def panics on disabled feature flag | medium | 🟢 |
| P2-9 | Algorithm | compute_scores_opt empty-tokenization 0.5 fallback in mixed cohort | medium | 🟢 |
| P2-10 | Algorithm | SPLADE min-max normalization collapse on negative cohort | medium | ✅ |
| P2-11 | Algorithm | total_cmp tie-break interleaves 0.5 fallback (companion to P2-9) | medium | 🟢 |
| P2-12 | Platform | worktree::lookup_main_cqs_dir asymmetric canonicalization | medium | ✅ |
| P2-13 | Platform | 5 strip_prefix sites leak abs paths on case-insensitive FS | medium | ✅ |
| P2-14 | Performance | ChunkRow::from_row 16 column-name strcmps per row | medium | ⏳ |

## P3 — Fix If Time (clusters listed; details in audit-findings.md)

| Category | Count | Notes |
|----------|-------|-------|
| Code Quality | 3 | CQ-3 (legacy NL wrapper deprecation), CQ-4 (stale dead-code reports — re-index issue), CQ-5 (NL gen wrapper proliferation) |
| API Design | 8 | project/ref/index/onboard ergonomics, --limit defaults, BatchProvider builder, cross_project pub mod |
| Error Handling | 8 | EH-1, 4, 5, 7, 8, 9, 10 — easy hardenings, low impact each |
| Observability | 8 | ✅ all shipped: hnsw verify span, validate_summary, compute_rewrite, find_ld, token_count, probe_model_vocab, worktree, cagra_persist (cagra one was P2-3) |
| TC Adversarial | 5 | aspx/build_hierarchy/parse_env/window edges/split_into_windows |
| TC Happy | 10 | All 10 — tests are net-positive but not load-bearing |
| Scaling | 7 | dim-blind batch sizes, query cache size, daemon clients cap, channel depth |
| Algorithm | 2 | ✅ select_negatives ordering, reverse_bfs guard |
| Extensibility | 4 | ✅ PoolingStrategy::Identity error; ⏳ synonyms / test_chunk / NEGATION / model-name validation |
| Platform | 1 | hardcoded /tmp in test fixtures |
| Security | 4 | open_browser cmd.exe, serve extractor errors, 64KiB pre-auth, daemon socket squat |
| Data Safety | 3 | ✅ INSERT OR REPLACE preventive, prune_old_backups read_dir; ⏳ HNSW load lock timeout |
| Performance | 11 | ✅ shipped: -2, -4, -5, -8, -9, -11; analysed/skipped: -1 (sort would be slower), -3 (= P2-14 cascade), -6 (visited_names cascade), -10 (current best), -12 (placeholder mismatch reverted) |
| Resource Mgmt | 4 | ✅ RM-V1.36-3, RM-V1.36-10; ⏳ RM-6, RM-8 |

## P4 — Defer (hard, design-level, or low-impact)

| ID | Category | Title | Status |
|----|----------|-------|--------|
| P4-1 | TC Adversarial | Concurrent writer contention path | hard, integration test |
| P4-2 | API Design | IndexBackend::try_open error semantics | medium, design |
| P4-3 | Extensibility | OutputFormat closed enum (12+ render sites) | medium-large refactor |
| P4-4 | Extensibility | classify_query_inner trait-ify | medium-large |
| P4-5 | Extensibility | Three near-identical prompt builders | medium-large |
| P4-6 | Extensibility | Backend selection policy hand-coded | medium |
| P4-7 | Platform | audit-mode no Windows ACL (already P4-19) | known |
| P4-8 | Platform | db_file_identity Windows mtime (already P4-17) | known |
| P4-9 | Platform | daemon_translate cfg(unix)-only — Windows daemon | hard |
| P4-10 | Platform | tasklist UTF-16 BOM | medium, edge case |
| P4-11 | Resource Mgmt | RM-V1.36-9 HNSW id_map Vec<String> | medium-large, future-scale |
| P4-12 | Data Safety | DS-V1.36-2 CAGRA save no .bak | medium, infrequent loss surface |
| P4-13 | Data Safety | DS-V1.36-5 SPLADE save no .bak | medium |
| P4-14 | Data Safety | DS-V1.36-6 HNSW load file-lock no timeout | medium |
| P4-15 | Security | open_browser cmd.exe metachar (latent) | medium |
| P4-16 | Security | serve query string in extractor errors | medium |
| P4-17 | Security | 64 KiB body limit pre-auth | medium |
| P4-18 | Security | Daemon socket cleanup TOCTOU | medium-hard |
