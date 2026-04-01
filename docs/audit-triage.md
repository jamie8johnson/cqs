# Audit Triage — v1.13.0

110 findings across 15 categories. 2026-03-31.

## P1: Easy + High Impact — Fix Immediately

| # | Finding | Status |
|---|---------|--------|
| RB-1 | `cmd_query` panics on multi-byte UTF-8 at byte 200 | ✅ PR #737 |
| RB-2 | `rerank_with_passages` assert_eq! panics on mismatched lengths | ✅ PR #737 |
| AC-2 | `score_candidate` negative scores invert note boost/demotion | ✅ PR #737 |
| AC-5 | `rrf_fuse` double-counts duplicate IDs in semantic list | ✅ PR #737 |
| AC-4 | `bfs_shortest_path` in trace — no node cap, unbounded memory | ✅ PR #737 |
| SHL-1 | MAX_TOKENS_PER_WINDOW=480 hardcoded — handicaps large-context models | ✅ PR #737 |
| SHL-7 | MAX_CONTRASTIVE_CHUNKS=15000 kills contrastive summaries for large codebases | ✅ PR #737 |
| DS-39 | `bytes_to_embedding` silently returns None on dim mismatch — empty search | ✅ PR #737 |
| SEC-1 | `cmd_index` creates .cqs dir without 0o700 permissions | ✅ PR #737 |
| SEC-2 | Telemetry file world-readable with user queries | ✅ PR #737 |
| DOC-1 | Language count says 51 in 7 locations — should be 52 | ✅ PR #737 |
| DOC-3 | lib.rs says E5-base is default — reversed since v1.9.0 | ✅ PR #737 |
| DOC-4 | PRIVACY.md says E5-base is default — reversed | ✅ PR #737 |
| EH-2 | `try_acquire_index_lock` treats I/O errors as "lock held" | ✅ PR #737 |
| EH-3 | CI `find_dead_code` failure shows "0 dead code" — false pass | ✅ PR #737 |

## P2: Medium Effort + High Impact — Fix in Batch

| # | Finding | Status |
|---|---------|--------|
| SHL-3 | NL description .take(1800) char budget assumes 512-token model | ✅ PR #737 |
| SHL-6 | HNSW params (M=24, ef=200) hardcoded for 10k-100k — no config override | |
| SHL-12 | EMBED_BATCH_SIZE=64 fixed regardless of GPU VRAM | |
| AC-1 | Cross-project RRF scores compared as absolute — invalid | |
| DS-38 | DEFERRED transactions + no process lock = SQLITE_BUSY on concurrent index | |
| DS-43 | Batch mode doesn't reload HNSW dim after config change | |
| SEC-3 | ensure_model doesn't verify joined paths stay inside CQS_ONNX_DIR | ✅ PR #738 |
| PERF-2 | upsert_fts_conditional: 2 SQL per chunk instead of batching (22K round trips) | ✅ PR #738 |
| PERF-6 | finalize_results clones ChunkRow for every search result | ✅ PR #738 |
| RM-5 | Contrastive neighbors allocates N*(N-1) pairs — 1.6GB intermediate | ✅ PR #738 |
| CQ-2 | dispatch_test_map duplicates 80-line reverse-BFS with divergent guard | |
| RB-5 | embed_batched panics on unexpected ONNX output shape | |
| EX-3 | Windowing pinned to 512 tokens — thread ModelConfig::max_seq_length | |
| PB-7 | rewrite_file locks source file — Windows mandatory lock hazard | |

## P3: Easy + Low Impact — Fix If Time

| # | Finding | Status |
|---|---------|--------|
| DOC-2 | mod.rs feature flag doc comments missing lang-st | ✅ PR #737 |
| DOC-5 | SECURITY.md missing v9-200k preset | ✅ PR #737 |
| DOC-6 | PRIVACY.md missing v9-200k preset | ✅ PR #737 |
| DOC-7 | README eval section cites 55-query eval | ✅ PR #737 |
| DOC-8 | Migration doc comment says "768-dim E5-base-v2" | ✅ PR #737 |
| DOC-9 | CHANGELOG missing IEC 61131-3 entry | ✅ PR #737 |
| DOC-10 | Cargo.toml description: "51 languages", stale eval metrics | ✅ PR #737 |
| EH-1 | serde_json::to_value().ok() drops errors in 7 locations | |
| EH-4 | build_brief_data degrades silently on failure | |
| EH-5 | set_permissions failures silently ignored in 6 locations | |
| EH-6 | index_notes parse error → Ok((0, false)) | |
| CQ-1 | dispatch_trace re-implements BFS instead of calling bfs_shortest_path | |
| CQ-3 | Impact test-suggestion JSON duplicated between cmd and batch | |
| CQ-4 | cmd_test_map computes unused _test_names HashSet | ✅ PR #737 |
| CQ-5 | cmd_trace uses N queries vs batch handler's batched query | |
| CQ-6 | filter_by_pattern dead code with #[allow(dead_code)] | ✅ PR #737 |
| OB-1 | plan() missing tracing span | ✅ PR #737 |
| OB-2 | create_client/LlmClient::new missing spans | ✅ PR #737 |
| OB-3 | delete_phantom_chunks missing span | ✅ PR #737 |
| OB-4 | search_filtered outer wrapper missing span | ✅ PR #737 |
| OB-5 | Batch get_ref missing timing | ✅ PR #737 |
| AC-3 | score_name_match missing "query contains name" tier | ✅ PR #737 |
| AC-6 | token_pack first-item override can exceed budget | ✅ PR #737 |
| RB-3 | Language::grammar() dead code that panics | ✅ PR #737 |
| RB-4 | Cli::model_config() panics on temporal coupling | |
| RB-6 | enrichment_hash unstable under LLM whitespace variation | |
| PB-1 | HNSW save lock truncates, load lock doesn't | ✅ PR #737 |
| PB-2 | collect_events non-canonicalized path comparison | |
| PB-3 | find_7z hardcodes Windows path | |
| PB-4 | Duplicate find_python in export_model and pdf | |
| PB-6 | ort_runtime_search_dir reads /proc/self/cmdline — no macOS fallback | |
| PERF-1 | find_test_chunks cache clones Vec<ChunkSummary> per call | |
| PERF-3 | Watch mode per-file upsert_type_edges instead of batched | ✅ PR #737 |
| PERF-4 | Watch all_calls linear scan per file | ✅ PR #737 |
| PERF-5 | upsert_chunks_and_calls individual DELETE per caller | ✅ PR #737 |
| SEC-5 | expand_query_for_fts no debug_assert on pre-sanitized input | |
| SEC-6 | search_by_name FTS query via format! with quotes | |
| TC-1 | Reranker: NaN passthrough, only 6 tests | |
| TC-2 | structured_text.rs: untested method/action/type queries | |
| TC-3 | rerank_with_passages assert panics (same as RB-2) | |
| TC-4 | score_candidate: no NaN embedding test | |
| DS-40 | Migration v15→v16 not idempotent on partial rollback | ✅ PR #737 |
| DS-41 | set_hnsw_dirty failure skips reindex permanently | |
| DS-42 | prune_all TOCTOU with concurrent file creation | |
| RM-1 | train_data dedup HashMap unbounded | |
| RM-3 | webhelp merged string no size limit | |
| RM-4 | Watch mtime pruning only at >10K entries | |
| RM-6 | Watch embedder retry clones ModelConfig per attempt | |
| SHL-2 | Reranker max_length=512 hardcoded | |
| SHL-8 | gather BFS 200-node cap regardless of codebase size | |
| SHL-9 | impact BFS 10K cap no relation to graph density | |
| SHL-10 | CAGRA 2GB RAM cap ignores actual available RAM | |
| SHL-11 | Rayon 4 threads hardcoded | |
| SHL-15 | Query cache 32 entries small for batch mode | |
| EX-4 | DEFAULT_NAME_BOOST etc scattered across 3 modules | |
| EX-5 | Watch mode constants not configurable | |
| EX-6 | Markdown section size limits hardcoded | |

## P4: Hard or Low Impact — Issues for Hard, Fix Trivials

| # | Finding | Status |
|---|---------|--------|
| CQ-7 | cmd/batch 2128-line duplication — systemic, needs shared serialization layer | |
| CQ-8 | Four clippy::too_many_arguments suppressions remain | |
| CQ-9 | lib.rs re-exports ~70 items | |
| EX-1 | New CLI command requires 5+ file changes | |
| EX-2 | HNSW build params not configurable (M, ef_construction) | |
| AD-1 | cmd_blame parameter ordering inconsistent | |
| AD-2 | name vs target inconsistent across 10+ commands | |
| AD-3 | root vs project_root naming split | |
| AD-4 | gather() takes pre-computed embedding vs peer functions taking embedder | |
| AD-5 | 26 commands use bare json: bool instead of OutputArgs | |
| AD-6 | ScoutOptions/PlacementOptions different default patterns | |
| AD-7 | embedding_slice returns Option where Result would be better | |
| AD-8 | analyze_impact takes 5 positional params vs options struct | |
| AD-9 | Embedding::new accepts any data without validation | |
| AD-10 | search_single_project returns Option hiding failures | |
| PB-5 | macOS case-fold diverges from APFS Unicode normalization | |
| SEC-4 | convert_directory output path not validated | |
| RM-2 | Bm25Index::build clones entire corpus | |
| TC-5 | suggest_placement_with_options zero direct tests | |
| SHL-4 | diff.rs comment "768 dims" | ✅ PR #737 |
| SHL-5 | embedder comment "[batch, seq_len, 768]" | ✅ PR #737 |
| SHL-13 | RRF K=60 not tuned for code search | |
| SHL-14 | DEFAULT_THRESHOLD=0.3 not documented as model-specific | |
