# Audit Triage — v0.14.0

Generated: 2026-02-22

## Summary

- **Total findings:** 61 (16 Batch 1 + 26 Batch 2 + 19 Batch 3)
- **Red team:** 4 categories, 3 actionable findings, filesystem boundaries fully verified clean
- **Batches:** 3 (Code Quality/Docs/API/EH/Obs, Tests/Algo/Ext/Platform, Red Team + Performance)

## P1: Easy + High Impact — Fix Immediately

| # | Finding | Difficulty | Location | Status |
|---|---------|-----------|----------|--------|
| 1 | **AD-4**: risk_level Debug vs Display (PascalCase vs lowercase in JSON) | easy | task.rs:255, cli/commands/task.rs:357 | ✅ fixed |
| 2 | **AC-1/AC-2**: Waterfall surplus overflow — token_count > token_budget | medium | cli/commands/task.rs:81-195 | ✅ fixed |
| 3 | **RT-DATA-5**: HNSW search NaN scores corrupt sort order | easy | hnsw/search.rs:49 | ✅ fixed |
| 4 | **RT-DATA-1**: Failed HNSW rebuild leaves stale .bin — silent result shrinkage | easy | cli/watch.rs:202-212 | ✅ fixed |
| 5 | **EH-1**: scout_with_options hard-fails on find_test_chunks (task degrades) | easy | scout.rs:127 | ✅ fixed |
| 6 | **EH-3**: unwrap() after is_none() guard — violates no-unwrap convention | easy | batch/handlers.rs:1037,1124 | ✅ fixed |
| 7 | **CQ-1**: task_to_json duplicates notes in JSON output | easy | task.rs:227-322 | ✅ fixed |
| 8 | **AC-4**: dedup_tests by name only — same-name tests in different files collapse | easy | task.rs:184-192 | ✅ fixed |
| 9 | **AC-6**: compute_modify_threshold returns 0.0 when all test chunks — everything becomes ModifyTarget | easy | scout.rs:309-317 | ✅ fixed |

## P2: Medium Effort + High Impact — Fix in Batch

| # | Finding | Difficulty | Location | Status |
|---|---------|-----------|----------|--------|
| 1 | **CQ-2**: JSON serialization duplicated 3-6x (root cause of AD-4) | medium | task.rs, cli/commands/task.rs, batch/handlers.rs, gather.rs, impact/format.rs | ✅ fixed |
| 2 | **CQ-3/RT-RES-9**: dispatch_task bypasses BatchContext caches | easy | batch/handlers.rs:1446-1496 | ✅ fixed |
| 3 | **CQ-4/EH-5**: Batch dispatch_task token budgeting diverges from CLI waterfall | medium | batch/handlers.rs:1459 vs cli/commands/task.rs:67-228 | ✅ fixed |
| 4 | **RT-DATA-4**: Batch call_graph cache inconsistent with task() fresh load | medium | batch/mod.rs:173-182 | ✅ fixed |
| 5 | **PF-1**: Re-embed query in placement phase — redundant ONNX inference | medium | task.rs:136, where_to_add.rs:115 | ✅ fixed |
| 6 | **PF-2**: Duplicate reverse_bfs across impact and test discovery | medium | task.rs:124,132 | ✅ fixed |
| 7 | **EX-1**: Waterfall percentages as magic numbers in 5+ locations | easy | cli/commands/task.rs:84,106,117,160,184 | ✅ fixed |
| 8 | **TC-1/TC-9**: task() and cqs task have no integration tests | medium | task.rs, cli_commands_test.rs | ✅ fixed |
| 9 | **TC-7**: Retrieval metrics have no unit tests | medium | tests/model_eval.rs:1244-1407 | ✅ fixed |

## P3: Easy + Low Impact — Fix If Time

| # | Finding | Difficulty | Location | Status |
|---|---------|-----------|----------|--------|
| 1 | **AD-1**: TaskResult/TaskSummary missing Debug/Clone derives | easy | task.rs:20-45 | ✅ fixed |
| 2 | **AD-2**: ScoutChunk/FileGroup/ScoutSummary/ScoutResult missing derives | easy | scout.rs:26-70 | ✅ fixed |
| 3 | **AD-3**: PlacementResult/FileSuggestion etc missing derives | easy | where_to_add.rs:21-53, scout.rs:84 | ✅ fixed |
| 4 | **AD-5**: ScoutChunk u64 vs usize for counts | easy | scout.rs:38-40 | ✅ fixed |
| 5 | **OB-1**: compute_modify_threshold result not logged | easy | scout.rs:308 | ✅ fixed |
| 6 | **OB-2**: scout_core missing search result count in logs | easy | scout.rs:154-299 | ✅ fixed |
| 7 | **OB-3**: dispatch_task batch token budgeting not logged | easy | batch/handlers.rs:1460-1493 | ✅ fixed (P2) |
| 8 | **EH-2**: dispatch_test_map unwrap_or_default without tracing | easy | batch/handlers.rs:644-647 | ✅ fixed |
| 9 | **EH-4/PB-2**: scout_core to_str().unwrap_or("") — empty string for non-UTF8 | easy | scout.rs:202,223 | ✅ fixed |
| 10 | **AC-3**: index_pack includes first item with budget=0 | easy | cli/commands/task.rs:56 | ✅ fixed |
| 11 | **AC-5**: compute_modify_threshold doc inaccuracy (tied scores) | easy | scout.rs:336-337 | ✅ fixed |
| 12 | **CQ-5**: print_code_section_idx double-iterates content lines | easy | cli/commands/task.rs:554-559 | ✅ fixed |
| 13 | **PF-4**: Waterfall budgeting clones code content strings unnecessarily | easy | cli/commands/task.rs:107 | ✅ fixed |
| 14 | **PF-6**: dispatch_task serializes all code then overwrites with budgeted subset | easy | batch/handlers.rs:1457,1490 | ✅ fixed (P2) |
| 15 | **EX-3**: ChunkRole string serialization duplicated in 4 match arms | easy | scout.rs:411, cli/commands/task.rs:297,515, cli/commands/scout.rs:132 | ✅ fixed |
| 16 | **RT-DATA-6**: partial_cmp unwrap_or(Equal) — use f32::total_cmp instead | easy | store/mod.rs:674, store/notes.rs:164, diff.rs:181, search.rs:714 | ✅ fixed |
| 17 | **RT-INJ-9**: cmd_ref_add validates name late — confusing error | easy | cli/commands/reference.rs:64-89 | ✅ fixed |
| 18 | **RT-RES-3**: --tokens 0 emits content despite zero budget | easy | cli/commands/task.rs:56 | ✅ fixed |
| 19 | **RT-RES-8**: dispatch_test_map chain loop lacks iteration bound | easy | batch/handlers.rs:638-648 | ✅ fixed |
| 20 | **TC-4**: compute_modify_threshold untested with all-test-chunk inputs | easy | scout.rs:308-341 | ✅ fixed |
| 21 | **TC-6**: classify_role untested at exact threshold with test names | easy | scout.rs:344-352 | ✅ fixed |
| 22 | **TC-8**: index_pack untested with zero budget | easy | cli/commands/task.rs:36-64 | ✅ fixed |
| 23 | **TC-10**: note_mention_matches_file untested with empty strings | easy | scout.rs:383-391 | ✅ fixed |
| 24 | **RT-INJ-4**: validate_ref_name doesn't reject null bytes | easy | reference.rs:209-220 | ✅ fixed |
| 25 | **RT-DATA-7**: rewrite_notes_file reads from separate fd while holding lock | easy | note.rs:185-222 | ✅ fixed |

## P4: Hard or Low Impact — Create Issues

| # | Finding | Difficulty | Location | Status |
|---|---------|-----------|----------|--------|
| 1 | **EH-6**: AnalysisError lacks general phase failure variant | easy | lib.rs:142-149 | |
| 2 | **EX-2**: Task BFS gather params hardcoded inline | easy | task.rs:103-106 | |
| 3 | **EX-4**: task() test depth hardcoded to 5 | easy | task.rs:186 | |
| 4 | **EX-5**: Batch dispatch requires 3-4 file changes per command | easy | batch/ | |
| 5 | **EX-6**: MIN_GAP_RATIO not exposed in ScoutOptions | easy | scout.rs:75 | |
| 6 | **EX-7**: TaskResult fixed struct — adding section touches 7 locations | medium | task.rs:20-35 | |
| 7 | **TC-2**: dedup_tests tested via simulation, not actual function | easy | task.rs:534-571 | |
| 8 | **TC-3**: task_to_json tests check structure but not values | easy | task.rs:465-495 | |
| 9 | **TC-5**: scout_core() has no integration test | medium | scout.rs:145-299 | |
| 10 | **TC-11**: Waterfall surplus forwarding logic untested | medium | cli/commands/task.rs:84-184 | |
| 11 | **PB-1**: is_test_chunk forward-slash only patterns | easy | lib.rs:201-207 | |
| 12 | **PF-3**: scout_core calls reverse_bfs per chunk (15x BFS) | medium | scout.rs:228-233 | |
| 13 | **PF-5**: find_relevant_notes O(N*M*F) with per-call allocations | easy | scout.rs:368-375 | |
| 14 | **RT-DATA-2**: GC orphan vectors between prune and rebuild | low | cli/commands/gc.rs:35-51 | |
| 15 | **RT-DATA-3**: Watch reindex + concurrent search sees partial state | low | cli/watch.rs:190 | |
| 16 | **RT-DATA-8**: Embedding::new() bypasses dimension validation | easy | embedder.rs:87-89 | |
| 17 | **RT-INJ-7**: CQS_PDF_SCRIPT .py extension check not implemented (SEC-8 gap) | easy | convert/pdf.rs:54-63 | |
| 18 | **RT-RES-1**: Pipeline intermediate merge unbounded before truncation | easy | batch/pipeline.rs:260-275 | |

## Red Team Summary

| Category | Targets Examined | Findings | Clean |
|---|---|---|---|
| RT-INJ (Input Injection) | 9 | 2 low + 1 UX | 6 verified safe |
| RT-FS (Filesystem Boundary) | 5 | 0 | 5 verified safe |
| RT-RES (Adversarial Robustness) | 13 | 1 medium + 2 low | 8 verified safe, 2 cross-refs |
| RT-DATA (Silent Data Corruption) | 9 | 3 medium + 4 low | 1 verified safe |

**Strongest defenses:** FTS5 sanitization (all paths covered), path traversal (all paths covered), graph cycle handling (all 5 BFS implementations have visited sets), batch line limits, tokenizer truncation.

**Weakest area:** Data integrity under concurrent access and cache consistency in batch mode.

## Cross-References

| Finding | Cross-ref | Note |
|---|---|---|
| CQ-4/EH-5 | Same issue | Batch vs CLI waterfall divergence |
| CQ-3/RT-RES-9 | Same issue | dispatch_task bypasses BatchContext cache |
| AC-1/RT-RES-13 | Same issue | Waterfall surplus overflow |
| AC-3/RT-RES-3 | Related | index_pack budget=0 behavior |
| AD-4/CQ-2/EX-3 | Related chain | Debug format → JSON duplication → ChunkRole duplication |
| EH-4/PB-2 | Related | to_str vs to_string_lossy inconsistency |
| RT-DATA-5/RT-DATA-6 | Related | NaN in HNSW → corrupt sorting |
| PF-1 | Related to CQ-3 | Both involve redundant work in task() |
