# Audit Triage — v0.28.1+uncommitted

Date: 2026-03-06

## Already Fixed During Audit

| # | Finding | Category | Status |
|---|---------|----------|--------|
| 1 | EH-4: `StoreError::Runtime` catch-all (8→1 sites) | Error Handling | fixed |
| 2 | PB-7: `cmd_watch` no poll fallback on WSL | Platform | fixed |

## P1: Easy + High Impact — Fix Immediately

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 1 | DS-8: GC uses wrong HNSW filename `id_map.json` → should use `HNSW_ALL_EXTENSIONS` | Data Safety | gc.rs:71 | fixed |
| 2 | RB-1: `to_lowercase()` byte offset bug in razor.rs — panics on non-ASCII | Robustness | razor.rs:197 | fixed |
| 3 | RB-2: `to_lowercase()` byte offset bug in latex.rs — same panic | Robustness | latex.rs:143 | fixed |
| 4 | PB-2: `enumerate_files` case-sensitive extension matching — skips `.RS`, `.Py` | Platform | lib.rs:371 | fixed |
| 5 | RB-3: `normalize_lang` missing `ini` and `markdown` | Robustness | markdown.rs:957 | fixed |
| 6 | AC-1: `search_by_name` results not sorted by name-match score | Algorithm | store/mod.rs:622 | fixed |
| 7 | AC-3: `EmbeddingBatchIterator::next()` recursion — stack overflow risk | Algorithm | chunks.rs:1544 | fixed |
| 8 | CQ-1: Dead code — `get_chunks_by_name` (singular), zero callers | Code Quality | chunks.rs:970 | fixed |
| 9 | CQ-2: Dead code — `search_chunks_by_signature`, zero callers | Code Quality | chunks.rs:1041 | fixed |
| 10 | EH-1/RB-5: `parse_fenced_blocks` silently skips 3 failure modes | Error/Robustness | mod.rs:618 | fixed |
| 11 | EH-3: `config.rs` loses error chain via `map_err` instead of `with_context` | Error Handling | config.rs:294 | fixed |
| 12 | OB-12: `find_injection_ranges` uses `info_span!` in hot loop — should be `debug_span!` | Observability | injection.rs:55 | fixed |
| 13 | RB-4: Unclosed fence silently eats rest of file — no diagnostic | Robustness | markdown.rs:1047 | fixed |
| 14 | PB-8: Dead backslash path checks in `is_test_chunk` | Platform | lib.rs:212 | fixed |
| 15 | RB-7: `is_recursive` joins all lines — unnecessary O(n) allocation | Robustness | structural.rs:189 | fixed |
| 16 | AD-1: CLI vs batch default `--limit` divergence | API Design | mod.rs/commands.rs | fixed |
| 17 | DOC-1: Language count 49→50 (Vue) across all docs | Documentation | multiple | fixed |
| 18 | DOC-2: Feature flag doc comment missing 10 language flags | Documentation | mod.rs:7 | fixed |

## P2: Medium Effort + High Impact — Fix in Batch

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 1 | PF-1: `get_call_graph()` uncached — 15 call sites, full table scan each | Performance | calls.rs:411 | fixed |
| 2 | PF-2: `find_test_chunks()` uncached — 14 call sites, full table scan each | Performance | calls.rs:1054 | fixed |
| 3 | PF-3: `analyze_impact` double table scan (auto-fixed by PF-1+PF-2) | Performance | analysis.rs:29 | fixed |
| 4 | DS-9: `config.rs` TOCTOU — reads via separate call after locking | Data Safety | config.rs:276 | fixed |
| 5 | AD-3: 5 store types missing `Serialize` | API Design | helpers.rs/calls.rs | fixed |
| 6 | AD-5: 5 command handlers accept unused `_cli: &Cli` | API Design | multiple | non-issue |
| 7 | CQ-3: CAGRA/HNSW index selection logic duplicated | Code Quality | query.rs/batch | fixed |
| 8 | AD-7: `DiffHunk` missing `Serialize` | API Design | diff_parse.rs:14 | fixed |
| 9 | AD-4/DOC-3: `diff_parse`, `drift`, `review` are `pub mod` — should be `pub(crate)` | API/Doc | lib.rs:67 | fixed |
| 10 | EH-2: `serde_json::to_value().ok()` silently drops chunks (4 locations) | Error Handling | multiple | fixed |
| 11 | SEC-1: `BufRead::lines()` allocates full line before size check — OOM on huge line | Security | batch/mod.rs:394 | fixed |
| 12 | EX-2: `chunk_importance` test detection hardcoded — not language-aware | Extensibility | search.rs:398 | fixed |
| 13 | CQ-4/EX-3/TC-2: `normalize_lang` no sync test, missing languages | Code Quality | markdown.rs:957 | fixed |

## P3: Easy + Low Impact — Fix If Time

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 1 | OB-5: `parse_markdown_chunks`/`parse_markdown_references` missing spans | Observability | markdown.rs | fixed |
| 2 | OB-6: `parse_notes`/`rewrite_notes_file` missing spans | Observability | note.rs | fixed |
| 3 | OB-7: HNSW build/save/load missing spans | Observability | hnsw/ | fixed |
| 4 | OB-8: CAGRA build/search missing spans | Observability | cagra.rs | fixed |
| 5 | OB-9: `load_references` missing span | Observability | reference.rs | fixed |
| 6 | OB-10: `enumerate_files` missing span | Observability | files.rs | fixed |
| 7 | OB-11: `extract_fenced_blocks` missing span | Observability | markdown.rs | fixed |
| 8 | CQ-5: 3 near-identical Chunk constructions in markdown | Code Quality | markdown.rs | fixed |
| 9 | DOC-4: CLAUDE.md skills list missing 5 skills | Documentation | CLAUDE.md | fixed |
| 10 | DOC-5: CHANGELOG [Unreleased] empty | Documentation | CHANGELOG.md | fixed |
| 11 | DOC-6: ROADMAP says 49 languages | Documentation | ROADMAP.md | non-issue (already says 50) |
| 12 | PB-1: Markdown parsers don't normalize CRLF | Platform | markdown.rs | fixed (doc comment) |
| 13 | EX-6: `name_match_score` magic numbers | Extensibility | search.rs:130 | fixed |
| 14 | EX-7: `chunk_importance` magic numbers | Extensibility | search.rs:398 | fixed |
| 15 | EX-1: `Pattern` enum 4 manual-sync representations | Extensibility | structural.rs | non-issue (existing tests adequate) |
| 16 | PB-4: `ensure_ort_provider_libs` misleading stub comment | Platform | embedder.rs | fixed |
| 17 | SEC-2: `is_webhelp_dir` follows symlinks in detection | Security | webhelp.rs:19 | fixed |
| 18 | SEC-3: SECURITY.md threat model understates trust levels | Security | SECURITY.md | fixed |
| 19 | DS-12: Second-precision mtime — sub-second edits missed | Data Safety | notes.rs:234 | non-issue (acceptable limitation) |
| 20 | DS-13: `count_vectors` reads HNSW IDs without lock | Data Safety | persist.rs:442 | fixed |
| 21 | PF-5: FTS INSERT per-row despite bulk DELETE | Performance | chunks.rs:290 | fixed |
| 22 | PF-6: `count_stale_files`/`list_stale_files` duplicate SQL | Performance | chunks.rs:546 | fixed |
| 23 | TC-3: `extract_fenced_blocks` missing edge case tests | Test Coverage | markdown.rs | fixed (4 tests) |
| 24 | TC-4: `build_risk_summary` never directly tested | Test Coverage | review.rs | fixed (3 tests) |
| 25 | TC-5: `match_notes` only one happy path test | Test Coverage | review.rs | deferred (needs Store, integration test exists) |
| 26 | TC-8: `run_ci_analysis` dead code path untested edge cases | Test Coverage | ci.rs | deferred (CLI handler, low value) |
| 27 | AD-2: `SearchResult` dual serialization shapes | API Design | helpers.rs | fixed |
| 28 | AC-2: `apply_token_budget` `.max(1)` exceeds budget | Algorithm | review.rs:97 | fixed (warning message) |
| 29 | AC-4: `index_pack`/`token_pack` first-item guarantee — by design | Algorithm | task.rs:62 | non-issue (by design) |
| 30 | PB-6: Path traversal check case-sensitivity — doc gap | Platform | read.rs:35 | fixed (doc comment) |
| 31 | RM-4: `index_notes_from_file` creates separate Embedder | Resource | index.rs:269 | non-issue (low impact, shared Embedder complicates API) |
| 32 | RM-6: `HnswIndex::build` 2x memory (test-only) | Resource | build.rs:67 | non-issue (test-only path) |
| 33 | RM-7: `last_indexed_mtime` unbounded growth | Resource | watch.rs:119 | fixed (comment + with_capacity) |
| 34 | RM-8: SQLite 64MB page cache potential | Resource | mod.rs:217 | non-issue (typical usage fine) |

## P4: Hard or Low Impact — Defer / Create Issues

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 1 | OB-1: store/calls.rs 15 functions missing spans | Observability | calls.rs | fixed |
| 2 | OB-2: store/types.rs 9 functions missing spans | Observability | types.rs | fixed |
| 3 | OB-3: store/notes.rs 7 functions missing spans | Observability | notes.rs | fixed |
| 4 | OB-4: store/chunks.rs 16 functions missing spans | Observability | chunks.rs | fixed |
| 5 | TC-1: 9 languages missing parser integration tests | Test Coverage | parser_test.rs | fixed |
| 6 | TC-6: Fenced block call-graph untested | Test Coverage | mod.rs:361 | fixed (documented limitation) |
| 7 | TC-7: handlers.rs 1306 lines zero inline tests | Test Coverage | handlers.rs | deferred (17 integration tests adequate) |
| 8 | RB-6: store/chunks.rs 16+ panicking `row.get()` | Robustness | chunks.rs | fixed |
| 9 | EX-4: `where_to_add` catch-all for 42 languages | Extensibility | where_to_add.rs | deferred (advisory feature, low value) |
| 10 | EX-5: HNSW ef_search compile-time only | Extensibility | hnsw/mod.rs | deferred (defaults work for 10k-100k range) |
| 11 | AD-6: Inconsistent `_with_*` naming convention | API Design | multiple | deferred (cosmetic, no functional impact) |
| 12 | PB-3: `find_project_root` walks to filesystem root | Platform | config.rs:28 | fixed |
| 13 | PB-5: `ProjectRegistry::save()` NTFS advisory lock | Platform | project.rs:56 | deferred (rare concurrent scenario) |
| 14 | DS-10: `rewrite_notes_file` copy fallback non-atomic | Data Safety | note.rs:264 | deferred (cross-device edge case) |
| 15 | DS-11: `extract_relationships` not transactional with chunks | Data Safety | index.rs:124 | deferred (accepted in prior audit) |
| 16 | PF-4: `search_across_projects` serial | Performance | project.rs:172 | fixed |
| 17 | RM-1: CAGRA dataset CPU-side retention (existing #389) | Resource | cagra.rs:64 | deferred (existing #389) |
| 18 | RM-2: Watch rebuilds full HNSW on every change | Resource | watch.rs:324 | deferred (needs incremental HNSW design) |
| 19 | RM-3: `BatchContext` caches never released | Resource | batch/mod.rs:55 | deferred (short-lived sessions) |
| 20 | RM-5: `extract_relationships` double I/O | Resource | index.rs:193 | deferred (needs pipeline rework) ||
