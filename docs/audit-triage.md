# Audit Triage — v0.26.0

Audit date: 2026-03-06. 14 categories, 3 batches, ~50 unique findings (some overlap across categories).

## P1: Easy + High Impact — Fix Immediately

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 1 | `"section"` capture missing from `calls.rs` and `injection.rs` — LaTeX sections get wrong ChunkType in call graph | API Design, Code Quality | calls.rs:276, injection.rs:363 | ✅ fixed |
| 2 | Capture-name→ChunkType match duplicated 3x, diverges — extract shared `capture_name_to_chunk_type()` | Code Quality, API Design | calls.rs:314, injection.rs:400, chunk.rs:18 | ✅ fixed |
| 3 | `parse_file_relationships` doesn't remove outer container call/type entries when injection succeeds — dangling `function_calls` rows | Data Safety | calls.rs:388-407 | ✅ fixed |
| 4 | CSS injection test has vacuous `if !css_chunks.is_empty()` guard — passes even if CSS injection is broken | Test Coverage | html.rs tests | ✅ fixed |
| 5 | `detect_script_language` parses non-JS types (application/ld+json, x-shader) as JavaScript — wasted inner parse | Algorithm Correctness | html.rs detect_script_language | ✅ fixed |
| 6 | Uppercase file extensions (.HTML, .RS) silently rejected — real bug on case-insensitive FS | Platform Behavior | mod.rs:171, calls.rs:236, watch.rs:241 | ✅ fixed |
| 7 | `find_content_child` / `find_child_by_kind` identical across injection.rs and html.rs — extract shared helper | Code Quality | injection.rs:174, html.rs:127 | ✅ fixed |
| 8 | `MAX_FILE_SIZE` / `MAX_CHUNK_BYTES` inline constants at 4 sites — hoist to module-level | Code Quality | mod.rs:144, mod.rs:203, calls.rs:210, injection.rs:253 | ✅ fixed |
| 9 | `parse_injected_chunks` + `parse_injected_relationships` duplicate parser setup boilerplate — extract `build_injection_tree()` | Code Quality, API Design | injection.rs:206-230, 305-330 | ✅ fixed |
| 10 | `html.rs` module comment claims Svelte/Vue/Astro support that doesn't exist | Documentation | html.rs:4 | ✅ fixed |
| 11 | `parser/mod.rs` module comment omits `injection` and `markdown` submodules | Documentation | mod.rs:1-7 | ✅ fixed |
| 12 | CHANGELOG `[Unreleased]` empty — multi-grammar injection not documented | Documentation | CHANGELOG.md:8 | ✅ fixed |
| 13 | README HTML entry doesn't mention multi-grammar injection | Documentation | README.md:427 | ✅ fixed |

## P2: Medium Effort + High Impact — Fix in Batch

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 14 | Unbounded injection range count — crafted HTML with millions of tiny `<script>` blocks causes OOM | Security | injection.rs:83-94 | |
| 15 | `parse_injected_relationships` early-returns on call query failure, skipping independent type extraction | Robustness, Error Handling | injection.rs:340-346 | |
| 16 | `chunk_overlaps_container` is strict containment, not overlap — misnamed and will cause double-coverage for future hosts | Algorithm Correctness, Data Safety | injection.rs:486-494 | |
| 17 | Chained injection silently ignored — `parse_injected_chunks` never checks inner language's injections (blocks PHP→HTML→JS) | Extensibility | injection.rs | |
| 18 | `extract_calls` silently discards `set_language` and parse failures without logging | Error Handling | calls.rs:30-37 | |
| 19 | `get_query`/`get_call_query`/`get_type_query` use `{:?}` instead of `{}` for error formatting | Error Handling | mod.rs:95, 113, 131 | |
| 20 | `parse_injected_relationships` `get_call_query` Err arm drops real errors with misleading comment | Error Handling | injection.rs:340-346 | |
| 21 | `parse_file_relationships` relies on undocumented invariant that empty query patterns compile | Error Handling | calls.rs:258 | |
| 22 | `find_content_child` returns only first matching child — split `raw_text` from error recovery skipped | Algorithm Correctness | injection.rs find_content_child | |
| 23 | `chunk_overlaps_container` has no unit tests — boundary conditions untested | Test Coverage | injection.rs:486-494 | |
| 24 | Injected type references (`ChunkTypeRefs`) never asserted in tests | Test Coverage | injection.rs tests | |
| 25 | `detect_script_language` — `type="text/typescript"` branch untested | Test Coverage | html.rs tests | |
| 26 | Temp files written with umask-derived permissions before `chmod` applied; `note.rs` never `chmod`s | Security | audit.rs:119, config.rs:332, note.rs:250 | |

## P3: Easy + Low Impact — Fix if Time

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 27 | `InjectionRule` lacks `Debug` — only pub struct without it | API Design | mod.rs:164 | |
| 28 | `walk_for_containers` cursor-advance idiom duplicated twice within itself | Code Quality | injection.rs:138-168 | |
| 29 | `InjectionGroup` grouping uses O(n) linear scan (fine for 2 rules, won't scale) | Code Quality | injection.rs:83-94 | |
| 30 | `parse_injected_chunks` span missing file path | Observability | injection.rs:199 | |
| 31 | Silent injection replacement — no log when outer chunks replaced | Observability | mod.rs:241-250 | |
| 32 | `parse_injected_chunks`/`parse_injected_relationships` don't log chunk/call count on success | Observability | injection.rs:288, 478 | |
| 33 | `find_injection_ranges` uses `debug_span` while callers use `info_span` | Observability | injection.rs:41 | |
| 34 | `scout_core` has no entry span | Observability | scout.rs:199 | |
| 35 | `cmd_read_focused` has no entry span | Observability | read.rs:312 | |
| 36 | Oversized injected chunks skipped silently — no debug log | Observability | injection.rs:252-256 | |
| 37 | Outer tree-sitter Parser/Tree not dropped before injection inner-parse | Resource Management | mod.rs:182-266, calls.rs:247-409 | |
| 38 | Call-dedup `HashSet<String>` should be `HashSet<&str>` — avoids clone per callee | Resource Management, Performance | injection.rs:447, calls.rs:352 | |
| 39 | `extract_types` builds `HashSet<String>` — can use `HashSet<&str>` | Performance | extract_types | |
| 40 | `capture_index_for_name("name")` called inside per-match hot loops — hoist out of loop | Performance | calls.rs:301, injection.rs:388, chunk.rs | |
| 41 | `extract_calls` missing CRLF normalization (all callers pass normalized, but latent) | Platform Behavior | calls.rs:15-78 | |
| 42 | `InjectionRule::target_language` convention unenforced (no compile/runtime validation) | Platform Behavior, Extensibility | mod.rs:172, injection.rs:63 | |
| 43 | `source[byte_range()]` direct indexing vs `utf8_text` — inconsistent style | Robustness | injection.rs:391, 434 | |
| 44 | `walk_for_containers` duplicate range risk if two rules share `container_kind` | Robustness | injection.rs:49-53 | |
| 45 | Grammar-less languages with non-empty `injections` field would silently produce nothing | Extensibility | mod.rs, injection.rs | |
| 46 | No contributor docs for adding new injection rules | Extensibility | CONTRIBUTING.md | |
| 47 | `run_git_log_line_range` doesn't validate colons in file path — git `-L` misparse | Security | blame.rs:79-93 | |
| 48 | No test for `type="text/typescript"` attribute detection | Test Coverage | html.rs tests | |

## P4: Hard or Low Impact — Defer / Create Issues

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 49 | Double-read + double-parse of every file during full index (parse_file + parse_file_relationships) | Performance | mod.rs, calls.rs | |
| 50 | Per-call Parser allocations in rayon parallel injection stage — memory amplification | Platform Behavior, Performance | injection.rs:206-210, 305-310 | |
| 51 | Two-pass write architecture (store → relationships) — crash between passes leaves stale edges | Data Safety | store/types.rs:106-184 | |
| 52 | Chunk ID collision between outer and injected chunks (theoretical, 32-bit hash) | Data Safety | chunk.rs:101 | |
| 53 | u32 arithmetic overflow in container_lines (theoretical, prevented by 50MB file limit) | Robustness | injection.rs:130-131 | |
| 54 | No end-to-end integration test (parse → embed → store → search for injected chunks) | Test Coverage | — | |
| 55 | No tests for malformed/unclosed `<script>` tags | Test Coverage | — | |
| 56 | Cross-phase line_start dependency between parse_file and parse_file_relationships undocumented | Algorithm Correctness | — | |
| 57 | `cursor.reset(root)` responsibility in caller, not callee — fragile | Algorithm Correctness | injection.rs walk_for_containers | |

## Overlap Notes

- P1#1 + P1#2 are the same root cause — fix together via shared `capture_name_to_chunk_type()`
- P2#15 + P2#20 are the same code location — fix together (split call/type error handling + add warning)
- P2#16 + P2#23 — rename + add tests together
- P3#38 + P3#39 — same pattern, fix together
- P3#42 + P3#45 — validation concerns, can add registry test for both
